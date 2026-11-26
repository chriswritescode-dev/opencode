pub mod handle;
pub mod remote;
pub mod sign_in;

use std::sync::Arc;
use std::time::Duration;

use opencode_config::entry::{Entry, Origin};
use opencode_contract::codec::encode_json;
use opencode_contract::generated::{
    ConnectionInfo, ConnectionStatus, CredentialValue, IntegrationID, IntegrationKeyMethod, IntegrationKeyMethodType,
    IntegrationMethod,
};

use crate::builtin::{Builtin, Services};
use crate::integration::{Implementation, StatusInput};
use crate::provider_plugin::PluginContext;

pub use self::handle::{Allowed, ConsoleHandle};

pub const ID: &str = "opencode.provider.opencode";

const INTEGRATION_ID: &str = "opencode";

const DEFAULT_MAX_AGE: Duration = Duration::from_secs(60);

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
}

pub async fn refresh(services: &Services, context: &PluginContext, max_age: Duration) {
    let _guard = context.console.loading.lock().await;
    let next = load(services, context, max_age).await;
    publish(services, context, next).await;
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
        Ok(None) => return Snapshot { remote: None, connection: Some(connection), organization: None },
        Err(error) => {
            return failed(services, context, connection, remote::ConsoleError::Authorization(error)).await;
        }
    };
    if matches!(credential, CredentialValue::External(_)) {
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

async fn publish(services: &Services, context: &PluginContext, next: Snapshot) {
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
    context.console.set_snapshot(next);
    context.console.bump();
}

fn connection_json(connection: &ConnectionInfo) -> serde_json::Value {
    serde_json::from_str(&encode_json(connection)).expect("a connection encodes as JSON")
}
