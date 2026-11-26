pub mod models;

use std::convert::Infallible;

use opencode_contract::generated::{
    ConnectionInfo, CredentialValue, IntegrationEnvMethod, IntegrationEnvMethodType, IntegrationID, IntegrationMethod,
    ModelInfo, ProviderID,
};

use crate::builtin::{Builtin, Services};
use crate::integration::Implementation;
use crate::provider_plugin::settings;
use crate::provider_plugin::{PluginContext, env_value, refresh};

pub const ID: &str = "opencode.provider.modal";

const PROVIDER_ID: &str = "modal";

#[derive(Clone, Debug, Default, PartialEq)]
struct Discovery {
    base_url: Option<String>,
    models: Option<Vec<models::RemoteModel>>,
    connection: Option<ConnectionInfo>,
}

pub async fn register(services: &Services, context: &PluginContext, builtin: &mut Builtin) {
    let provider_id = ProviderID(PROVIDER_ID.to_owned());
    let integration_id = IntegrationID(PROVIDER_ID.to_owned());

    let integration_registration = services
        .integration
        .transform(|editor| {
            editor.method_update(Implementation::Method {
                integration_id: IntegrationID(PROVIDER_ID.to_owned()),
                method: IntegrationMethod::Env(IntegrationEnvMethod {
                    r#type: IntegrationEnvMethodType::Env,
                    names: vec!["MODAL_PROXY_TOKEN".to_owned()],
                }),
            });
            Ok(())
        })
        .await;
    builtin.registrations.push(integration_registration);

    let integrations = services.integration.clone();
    let providers = services.provider.clone();
    let http = context.http.clone();
    let env = context.env.clone();
    let cache = context.shared_cache.clone();
    let reload = services.provider.clone();
    let active_integrations = integrations.clone();
    let active_integration_id = integration_id.clone();
    let load_provider_id = provider_id.clone();
    let load_integration_id = integration_id.clone();

    let discovered = refresh::start(
        &services.bus,
        refresh::Start::<Discovery, Infallible> {
            integration_id: PROVIDER_ID.to_owned(),
            initial: Discovery::default(),
            interval: None,
            active: Box::new(move || {
                let integrations = active_integrations.clone();
                let integration_id = active_integration_id.clone();
                Box::pin(async move { integrations.connection().active(&integration_id).await.ok().flatten() })
            }),
            load: Box::new(move |_, options| {
                let integrations = integrations.clone();
                let providers = providers.clone();
                let http = http.clone();
                let env = env.clone();
                let cache = cache.clone();
                let provider_id = load_provider_id.clone();
                let integration_id = load_integration_id.clone();
                Box::pin(async move {
                    let (connection, credential) = settings::active_credential(&integrations, &integration_id).await;
                    let api_key = match credential {
                        Some(CredentialValue::Key(key)) => Some(key.key),
                        _ => env_value(&env, "MODAL_PROXY_TOKEN"),
                    };
                    let provider = providers.get(&provider_id).await.ok().flatten();
                    let base_url = provider.as_ref().and_then(super::base_url).map(str::to_owned);
                    let (Some(api_key), Some(base_url)) = (api_key, base_url) else {
                        return Ok(refresh::Loaded { connection, value: Discovery::default() });
                    };
                    let key = models_key(connection.as_ref(), &api_key, &base_url);
                    let models = cache
                        .get(&key, options.interval, options.force, {
                            let integrations = integrations.clone();
                            let http = http.clone();
                            let env = env.clone();
                            let connection = connection.clone();
                            let base_url = base_url.clone();
                            move || {
                                let integrations = integrations.clone();
                                let http = http.clone();
                                let env = env.clone();
                                let connection = connection.clone();
                                let base_url = base_url.clone();
                                async move {
                                    let resolved = match &connection {
                                        Some(connection) => {
                                            integrations.connection().resolve(connection).await.ok().flatten()
                                        }
                                        None => None,
                                    };
                                    let key = match resolved {
                                        Some(CredentialValue::Key(key)) => Some(key.key),
                                        _ if connection.is_none() => env_value(&env, "MODAL_PROXY_TOKEN"),
                                        _ => None,
                                    };
                                    let Some(key) = key else {
                                        return Err(models::LoadError::MissingKey);
                                    };
                                    let mut models = models::load(http.as_ref(), &base_url, &key).await?;
                                    models.sort_by(|left, right| left.id.cmp(&right.id));
                                    Ok(models)
                                }
                            }
                        })
                        .await
                        .ok();
                    Ok(refresh::Loaded {
                        connection: connection.clone(),
                        value: Discovery { base_url: Some(base_url), models, connection },
                    })
                })
            }),
            reload: Box::new(move || {
                let providers = reload.clone();
                Box::pin(async move { providers.reload().await })
            }),
        },
    );

    let registration = services
        .provider
        .transform(move |editor| {
            let Some(item) = editor.get(&provider_id) else {
                return Ok(());
            };
            let provider = item.provider.clone();
            let existing: Vec<ModelInfo> = item.models.values().cloned().collect();
            let Discovery { base_url, models, connection } = discovered.current();
            let (Some(models), Some(base_url)) = (models, base_url) else {
                return Ok(());
            };
            let derived: Vec<ModelInfo> = models::derive(&base_url, &models, &existing).into_values().collect();
            editor.add(provider, &derived, connection);
            Ok(())
        })
        .await;
    builtin.registrations.push(registration);
}

pub fn models_key(connection: Option<&ConnectionInfo>, api_key: &str, base_url: &str) -> String {
    let access = connection.map(crate::integration::key).unwrap_or_else(|| "env:MODAL_PROXY_TOKEN".to_owned());
    format!("modal:{access}:{}:{}", crate::shared_cache::fingerprint(api_key), base_url.trim_end_matches('/'))
}
