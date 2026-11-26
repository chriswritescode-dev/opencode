pub mod follow;
pub mod handle;
pub mod remote;
pub mod sign_in;
pub mod websearch;

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::FutureExt;
use opencode_config::entry::{Entry, Origin};
use opencode_contract::codec::encode_json;
use opencode_contract::generated::{
    ConnectionInfo, ConnectionStatus, CredentialValue, IntegrationID, IntegrationKeyMethod, IntegrationKeyMethodType,
    IntegrationMethod, ProviderID, ProviderInfoActivation,
};

use crate::builtin::{Builtin, Services};
use crate::integration::{Implementation, StatusInput};
use crate::provider_plugin::PluginContext;
use crate::provider_plugin::hooks::{HookRegistration, HttpResponse};
use crate::provider_plugin::http::BufferedResponse;

use super::{env_value, setting};

pub use self::handle::{Allowed, ConsoleHandle};
pub use self::websearch::ConsoleWebSearch;

pub const ID: &str = "opencode.provider.opencode";

const INTEGRATION_ID: &str = "opencode";

const DEFAULT_MAX_AGE: Duration = Duration::from_secs(60);

const CHECK_MAX_AGE: Duration = Duration::from_secs(30);

const POLL_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub remote: Option<remote::RemoteConfig>,
    pub connection: Option<ConnectionInfo>,
    pub organization: Option<String>,
}

#[derive(Clone)]
enum Cached {
    Loaded(Option<Box<remote::RemoteConfig>>),
    Failed(remote::ConsoleError),
}

pub async fn register(services: &Services, context: &PluginContext, builtin: &mut Builtin) {
    context.console.set_web_search_dependencies(services.integration.clone(), context.http_manual_redirects.clone());
    let registration = services
        .integration
        .transform({
            let http = context.http.clone();
            let user_agent = context.app.useragent();
            move |editor| {
                let integration_id = IntegrationID(INTEGRATION_ID.to_owned());
                editor.update(&integration_id, |integration| integration.name = "OpenCode".to_owned());
                editor.method_update(Implementation::OAuth(Arc::new(sign_in::DeviceSignIn::new(
                    http.clone(),
                    user_agent.clone(),
                ))));
                editor.method_update(Implementation::Method {
                    integration_id,
                    method: IntegrationMethod::Key(IntegrationKeyMethod {
                        r#type: IntegrationKeyMethodType::Key,
                        label: Some("API key (service account)".to_owned()),
                        form: None,
                    }),
                });
                Ok(())
            }
        })
        .await;
    builtin.registrations.push(registration);

    refresh(services, context, DEFAULT_MAX_AGE).await;

    let provider_registration = services
        .provider
        .transform({
            let console = context.console.clone();
            move |editor| {
                let Some(info) = console.integrated() else {
                    return Ok(());
                };
                let connection = console.snapshot().connection;
                crate::builtin::config_provider::integrate(
                    editor,
                    &IntegrationID(INTEGRATION_ID.to_owned()),
                    &info,
                    connection.as_ref(),
                );
                Ok(())
            }
        })
        .await;
    builtin.registrations.push(provider_registration);

    let disable_free = {
        let value = (context.env)("OPENCODE_DISABLE_FREE_MODELS");
        opencode_config::options::truthy(value.as_deref())
    };
    let public_provider_registration = services
        .provider
        .transform({
            let env = context.env.clone();
            let console = context.console.clone();
            move |editor| {
                let provider_id = ProviderID(INTEGRATION_ID.to_owned());
                let has_key = {
                    let Some(item) = editor.get(&provider_id) else {
                        return Ok(());
                    };
                    env_value(&env, "OPENCODE_API_KEY").is_some()
                        || console.snapshot().connection.is_some()
                        || setting(item.provider.settings.as_ref(), "apiKey").is_some_and(|key| !key.is_empty())
                };
                if has_key || disable_free {
                    return Ok(());
                }
                editor.update(&provider_id, |provider| {
                    provider.activation = ProviderInfoActivation::Enabled;
                    let settings = provider.settings.get_or_insert_with(|| {
                        opencode_contract::codec::decode_json("{}").expect("empty provider settings")
                    });
                    match settings.rest.0.iter_mut().find(|(key, _)| key == "apiKey") {
                        Some((_, value)) => *value = serde_json::json!("public"),
                        None => settings.rest.0.push(("apiKey".to_owned(), serde_json::json!("public"))),
                    }
                });
                Ok(())
            }
        })
        .await;
    builtin.registrations.push(public_provider_registration);

    let public_model_registration = services
        .model
        .transform({
            let env = context.env.clone();
            let console = context.console.clone();
            move |editor| {
                let provider_id = ProviderID(INTEGRATION_ID.to_owned());
                let has_key = {
                    let Some(item) = editor.provider(&provider_id) else {
                        return Ok(());
                    };
                    let configured = setting(item.provider.settings.as_ref(), "apiKey")
                        .is_some_and(|key| !key.is_empty() && key != "public");
                    env_value(&env, "OPENCODE_API_KEY").is_some()
                        || console.snapshot().connection.is_some()
                        || configured
                };
                if has_key {
                    return Ok(());
                }
                let paid: Vec<String> = editor
                    .list(Some(&provider_id))
                    .into_iter()
                    .filter(|model| model.cost.iter().any(|cost| cost.input.0 > 0.0))
                    .map(|model| model.id.clone())
                    .collect();
                for model_id in paid {
                    editor.update(&provider_id, &model_id, |model| model.enabled = false);
                }
                Ok(())
            }
        })
        .await;
    builtin.registrations.push(public_model_registration);

    let switch = super::refresh::on_switch(&services.bus, INTEGRATION_ID, {
        let services = services.clone();
        let context = context.clone();
        move || {
            let services = services.clone();
            let context = context.clone();
            async move { refresh(&services, &context, DEFAULT_MAX_AGE).await }.boxed()
        }
    });
    builtin.tasks.push(switch);

    let poll = {
        let services = services.clone();
        let context = context.clone();
        tokio::spawn(async move {
            let mut changes = context.console.changes();
            loop {
                if context.console.snapshot().connection.is_none() {
                    if changes.changed().await.is_err() {
                        break;
                    }
                    continue;
                }
                tokio::select! {
                    _ = tokio::time::sleep(POLL_INTERVAL) => {
                        check(&services, &context, POLL_INTERVAL).await;
                    }
                    result = changes.changed() => {
                        if result.is_err() {
                            break;
                        }
                    }
                }
            }
        })
    };
    builtin.tasks.push(poll);
}

pub async fn refresh(services: &Services, context: &PluginContext, max_age: Duration) {
    let _guard = context.console.loading.lock().await;
    let next = load(services, context, max_age).await;
    apply(services, context, next).await;
}

async fn check(services: &Services, context: &PluginContext, max_age: Duration) {
    let _guard = context.console.loading.lock().await;
    let next = load(services, context, max_age).await;
    if next != context.console.snapshot() {
        apply(services, context, next).await;
    }
}

async fn apply(services: &Services, context: &PluginContext, next: Snapshot) {
    context.console.set_snapshot(next.clone());
    publish(services, context, &next).await;
    guard(services, context, &next);
    services.provider.reload().await;
    context.console.bump();
}

fn guard(services: &Services, context: &PluginContext, next: &Snapshot) {
    context.console.set_explainers(Vec::new());
    if next.connection.is_none() {
        return;
    }
    let mut provider_ids = vec![ProviderID(INTEGRATION_ID.to_owned())];
    if let Some(providers) = next.remote.as_ref().and_then(|remote| remote.info.providers.as_ref()) {
        for (id, _) in &providers.0 {
            let id = ProviderID(id.clone());
            if !provider_ids.contains(&id) {
                provider_ids.push(id);
            }
        }
    }
    let registrations: Vec<HookRegistration> = provider_ids
        .into_iter()
        .map(|provider_id| {
            let services = services.clone();
            let context = context.clone();
            let hooks = context.hooks.clone();
            hooks.on_http_response(Some(provider_id), move |event| {
                let services = services.clone();
                let context = context.clone();
                async move { explain(&services, &context, event).await }.boxed()
            })
        })
        .collect();
    context.console.set_explainers(registrations);
}

async fn explain(services: &Services, context: &PluginContext, mut event: HttpResponse) -> HttpResponse {
    if event.response.status != 401 && event.response.status != 403 {
        return event;
    }
    check(services, context, CHECK_MAX_AGE).await;
    let Some(status) = context.console.reported() else {
        return event;
    };
    let body = serde_json::json!({
        "error": {
            "type": "authentication_error",
            "message": format!("{}.", status.message),
        }
    });
    event.response = BufferedResponse {
        status: event.response.status,
        headers: vec![("content-type".to_owned(), "application/json".to_owned())],
        body: Bytes::from(serde_json::to_vec(&body).expect("the Console authentication error serializes")),
    };
    event
}

async fn load(services: &Services, context: &PluginContext, max_age: Duration) -> Snapshot {
    let integration_id = IntegrationID(INTEGRATION_ID.to_owned());
    let connections = services.integration.connection();
    let Some(connection) = connections.active(&integration_id).await.ok().flatten() else {
        context.console.set_reported(None);
        return Snapshot::default();
    };
    let credential = match connections.resolve(&connection).await {
        Ok(Some(credential)) => credential,
        Ok(None) => {
            report_status(services, context, &connection, None);
            return Snapshot { remote: None, connection: Some(connection), organization: None };
        }
        Err(error) => {
            return failed(services, context, connection, remote::ConsoleError::Authorization(error)).await;
        }
    };
    if matches!(credential, CredentialValue::External(_)) {
        report_status(services, context, &connection, None);
        return Snapshot { remote: None, connection: Some(connection), organization: None };
    }
    let organization = remote::organization_name(&credential);
    let workspace = crate::console_access::workspace_key(&connection, &credential);
    let key = format!("opencode-console:{workspace}");
    let http = context.http.clone();
    let user_agent = context.app.useragent();
    let cached = context
        .shared_cache
        .get::<Cached, remote::ConsoleError, _, _>(&key, Some(max_age), false, move || {
            let http = http.clone();
            let user_agent = user_agent.clone();
            let credential = credential.clone();
            async move {
                match remote::fetch_config(http.as_ref(), &user_agent, &credential).await {
                    Ok(config) => Ok(Cached::Loaded(config.map(Box::new))),
                    Err(error) if remote::connection_status(&error).is_some() => Ok(Cached::Failed(error)),
                    Err(error) => Err(error),
                }
            }
        })
        .await;
    match cached {
        Ok(Cached::Loaded(config)) => {
            report_status(services, context, &connection, None);
            Snapshot { remote: config.map(|config| *config), connection: Some(connection), organization }
        }
        Ok(Cached::Failed(error)) => failed(services, context, connection, error).await,
        Err(error) => failed(services, context, connection, error).await,
    }
}

async fn failed(
    services: &Services,
    context: &PluginContext,
    connection: ConnectionInfo,
    error: remote::ConsoleError,
) -> Snapshot {
    if let Some(status) = remote::connection_status(&error) {
        report_status(services, context, &connection, Some(status));
    }
    log::warn!("failed to load OpenCode provider config: {error:?}");
    let previous = context.console.snapshot();
    let keep = previous
        .connection
        .as_ref()
        .is_some_and(|previous| crate::integration::key(previous) == crate::integration::key(&connection));
    if keep {
        Snapshot { remote: previous.remote, connection: Some(connection), organization: previous.organization }
    } else {
        Snapshot { remote: None, connection: Some(connection), organization: None }
    }
}

fn report_status(
    services: &Services,
    context: &PluginContext,
    connection: &ConnectionInfo,
    status: Option<ConnectionStatus>,
) {
    context.console.set_reported(status.clone());
    let _ = services.integration.connection().status(StatusInput {
        integration_id: IntegrationID(INTEGRATION_ID.to_owned()),
        connection: connection.clone(),
        status,
    });
}

async fn publish(services: &Services, context: &PluginContext, next: &Snapshot) {
    let document = next.remote.as_ref().zip(next.connection.as_ref()).map(|(remote, connection)| {
        opencode_config::integration::make(connection_json(connection), next.organization.clone(), remote.info.clone())
    });
    let made = document.as_ref().map(|document| document.info.clone());
    services.config.integration(INTEGRATION_ID, document).await;
    let integrated = services.config.entries().into_iter().find_map(|entry| match entry {
        Entry::Document(document) => match &document.origin {
            Origin::Integration { integration_id, .. } if integration_id == INTEGRATION_ID => {
                Some(document.info.clone())
            }
            _ => None,
        },
        Entry::Directory { .. } => None,
    });
    let allowed = made.as_ref().is_some_and(|made| integrated.as_ref() == Some(made));
    let remote = next.remote.as_ref();
    context.console.set_integrated(integrated);
    context.console.set_allowed(Allowed {
        watches: allowed && remote.is_some_and(|remote| remote.watches),
        lists: allowed && remote.is_some_and(|remote| remote.lists),
        artifacts: allowed && remote.is_some_and(|remote| remote.artifacts),
    });
}

fn connection_json(connection: &ConnectionInfo) -> serde_json::Value {
    serde_json::from_str(&encode_json(connection)).expect("a connection encodes as JSON")
}
