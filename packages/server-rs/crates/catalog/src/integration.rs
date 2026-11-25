use std::collections::HashMap;
use std::ffi::OsString;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use indexmap::IndexMap;
use opencode_bus::{Bus, BusError, Definition, PublishOptions};
use opencode_contract::codec::encode_json;
use opencode_contract::generated::{
    ConnectionInfo, ConnectionStatus, CredentialOAuth, FormAnswer, IntegrationID, IntegrationInfo,
    IntegrationOAuthMethod, LocationRef,
};
use opencode_location::state::{self, BoxFuture, Options, Registration, State, TransformError};

use crate::credential::{self, CredentialError, CredentialService};

mod attempt;
mod command;
mod connect;
mod connection;
mod editor;

pub use attempt::{Attempt, AttemptError, AttemptMode, AttemptStatus, AttemptTime, OAuthConnectInput, OAuthService};
pub use command::{CommandAttempt, CommandAttemptStatus, CommandConnectInput, CommandError, CommandService};
pub use connect::{ExternalInput, KeyInput};
pub use connection::{ConnectionService, StatusInput, key};
pub use editor::{Editor, Implementation};

pub const UPDATED: &str = "integration.updated";

const SCRUB_INTERVAL: Duration = Duration::from_secs(30);

pub type ConfigEnv = Arc<dyn Fn(&str) -> Option<OsString> + Send + Sync>;

pub type CommandEnv = Arc<Vec<(OsString, OsString)>>;

pub type OAuthFuture<T> = BoxFuture<'static, Result<T, AuthorizationError>>;

pub type RefreshCallback = Box<dyn Fn(CredentialOAuth) -> OAuthFuture<CredentialOAuth> + Send + Sync>;

pub type RevokeCallback = Box<dyn Fn(CredentialOAuth) -> OAuthFuture<()> + Send + Sync>;

pub type LabelCallback = Box<dyn Fn(&CredentialOAuth) -> Option<String> + Send + Sync>;

pub type AutoCallback = Box<dyn FnOnce() -> OAuthFuture<CredentialOAuth> + Send>;

pub type SubmitCallback = Box<dyn Fn(String) -> OAuthFuture<()> + Send + Sync>;

pub type CodeCallback = Box<dyn Fn(String) -> OAuthFuture<CredentialOAuth> + Send + Sync>;

#[derive(Clone)]
pub struct AuthorizationError {
    cause: Option<Arc<dyn std::error::Error + Send + Sync>>,
}

impl AuthorizationError {
    pub fn new(cause: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self { cause: Some(Arc::new(cause)) }
    }

    pub fn from_shared(cause: Arc<dyn std::error::Error + Send + Sync>) -> Self {
        Self { cause: Some(cause) }
    }

    pub fn cause(&self) -> Option<&(dyn std::error::Error + Send + Sync)> {
        self.cause.as_deref()
    }

    pub fn message(&self) -> String {
        let message = self.cause.as_ref().map(ToString::to_string).unwrap_or_default();
        if message.is_empty() {
            return "Authorization failed".to_owned();
        }
        message
    }
}

impl std::fmt::Debug for AuthorizationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthorizationError").field("cause", &self.message()).finish()
    }
}

impl std::fmt::Display for AuthorizationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for AuthorizationError {}

pub enum OAuthMode {
    Auto { callback: AutoCallback, submit: Option<SubmitCallback> },
    Code { callback: CodeCallback },
}

pub struct OAuthAuthorization {
    pub url: String,
    pub instructions: String,
    pub expires_at: Option<i64>,
    pub mode: OAuthMode,
}

pub trait OAuthImplementation: Send + Sync {
    fn integration_id(&self) -> &IntegrationID;
    fn method(&self) -> &IntegrationOAuthMethod;
    fn authorize(&self, answer: FormAnswer) -> OAuthFuture<OAuthAuthorization>;
    fn refresh(&self) -> Option<RefreshCallback> {
        None
    }
    fn revoke(&self) -> Option<RevokeCallback> {
        None
    }
    fn label(&self) -> Option<LabelCallback> {
        None
    }
}

pub(crate) struct IntegrationEntry {
    pub(crate) info: IntegrationInfo,
    pub(crate) implementations: HashMap<String, Arc<dyn OAuthImplementation>>,
}

#[derive(Default)]
pub struct IntegrationRegistry {
    pub(crate) entries: IndexMap<String, IntegrationEntry>,
}

#[derive(Debug, thiserror::Error)]
pub enum IntegrationError {
    #[error(transparent)]
    State(#[from] opencode_location::state::StateError),
    #[error(transparent)]
    Credential(#[from] CredentialError),
    #[error(transparent)]
    Bus(#[from] BusError),
}

pub struct IntegrationService {
    pub(crate) state: State<IntegrationRegistry>,
    pub(crate) credentials: Arc<CredentialService>,
    pub(crate) bus: Arc<Bus>,
    pub(crate) location: LocationRef,
    pub(crate) environment: ConfigEnv,
    pub(crate) command_env: CommandEnv,
    pub(crate) statuses: Mutex<HashMap<String, ConnectionStatus>>,
    pub(crate) attempts: Mutex<HashMap<String, attempt::OAuthEntry>>,
    pub(crate) command_attempts: Mutex<HashMap<String, command::CommandEntry>>,
    cleanup: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl IntegrationService {
    pub fn new(
        credentials: Arc<CredentialService>,
        bus: Arc<Bus>,
        location: LocationRef,
        environment: ConfigEnv,
        command_env: CommandEnv,
    ) -> Arc<Self> {
        let notify_bus = bus.clone();
        let notify_location = location.clone();
        let notify: state::NotifyFn<IntegrationRegistry> = Box::new(move |_| {
            let bus = notify_bus.clone();
            let location = notify_location.clone();
            Box::pin(async move {
                let definition = Definition::ephemeral(UPDATED);
                let options = PublishOptions { location: Some(location), ..Default::default() };
                if let Err(error) = bus.publish(&definition, serde_json::json!({}), options) {
                    log::warn!("integration update event was not published: {error}");
                }
            })
        });
        let options = Options {
            name: Some("integration".to_owned()),
            initial: Box::new(IntegrationRegistry::default),
            notify: Some(state::changes(project_registry, notify)),
        };
        Arc::new_cyclic(|weak| {
            let cleanup = tokio::spawn(cleanup_task(weak.clone()));
            Self {
                state: State::new(options),
                credentials,
                bus,
                location,
                environment,
                command_env,
                statuses: Mutex::new(HashMap::new()),
                attempts: Mutex::new(HashMap::new()),
                command_attempts: Mutex::new(HashMap::new()),
                cleanup: Mutex::new(Some(cleanup)),
            }
        })
    }

    pub async fn transform<F>(&self, f: F) -> Registration
    where
        F: for<'a> Fn(&mut Editor<'a>) -> Result<(), TransformError> + Send + Sync + 'static,
    {
        self.state.transform(move |registry| f(&mut Editor { registry })).await
    }

    pub fn revision(&self) -> u64 {
        self.state.revision()
    }

    pub async fn reload(&self) {
        self.state.reload().await;
    }

    pub async fn get(&self, id: &IntegrationID) -> Result<Option<IntegrationInfo>, IntegrationError> {
        let registry = self.state.get().await?;
        let Some(entry) = registry.entries.get(&id.0) else {
            return Ok(None);
        };
        let saved = self.credentials.list(id)?;
        let connections = connection::resolve_connections(Some(entry), &saved, &self.environment);
        Ok(Some(self.project_entry(entry, connections)))
    }

    pub async fn list(&self) -> Result<Vec<IntegrationInfo>, IntegrationError> {
        let registry = self.state.get().await?;
        let mut groups: HashMap<String, Vec<credential::Info>> = HashMap::new();
        for saved in self.credentials.all()? {
            groups.entry(saved.integration_id.0.clone()).or_default().push(saved);
        }
        let mut entries: Vec<&IntegrationEntry> = registry.entries.values().collect();
        entries.sort_by(|a, b| a.info.name.cmp(&b.info.name));
        Ok(entries
            .into_iter()
            .map(|entry| {
                let saved = groups.get(&entry.info.id.0).map(Vec::as_slice).unwrap_or_default();
                let connections = connection::resolve_connections(Some(entry), saved, &self.environment);
                self.project_entry(entry, connections)
            })
            .collect())
    }

    pub fn connection(&self) -> ConnectionService<'_> {
        ConnectionService { service: self }
    }

    pub fn oauth(self: &Arc<Self>) -> OAuthService<'_> {
        OAuthService { service: self }
    }

    pub fn command(self: &Arc<Self>) -> CommandService<'_> {
        CommandService { service: self }
    }

    pub(crate) fn publish_updated(&self) -> Result<(), IntegrationError> {
        let definition = Definition::ephemeral(UPDATED);
        let options = PublishOptions { location: Some(self.location.clone()), ..Default::default() };
        self.bus.publish(&definition, serde_json::json!({}), options)?;
        Ok(())
    }

    pub(crate) fn project_entry(&self, entry: &IntegrationEntry, connections: Vec<ConnectionInfo>) -> IntegrationInfo {
        let statuses = self.statuses.lock().expect("lock integration statuses");
        let mut info = entry.info.clone();
        info.connections = connections
            .into_iter()
            .map(|connection| {
                let status = statuses.get(&status_key(&info.id, &connection)).cloned();
                connection::with_status(connection, status)
            })
            .collect();
        info
    }
}

pub(crate) fn status_key(integration_id: &IntegrationID, connection: &ConnectionInfo) -> String {
    format!("{}:{}", integration_id.0, connection::key(connection))
}

impl Drop for IntegrationService {
    fn drop(&mut self) {
        attempt::shutdown(self);
        command::shutdown(self);
        if let Ok(mut cleanup) = self.cleanup.lock()
            && let Some(task) = cleanup.take()
        {
            task.abort();
        }
    }
}

async fn cleanup_task(weak: Weak<IntegrationService>) {
    loop {
        tokio::time::sleep(SCRUB_INTERVAL).await;
        let Some(service) = weak.upgrade() else {
            return;
        };
        attempt::scrub(&service);
        command::scrub(&service);
    }
}

fn project_registry(registry: &IntegrationRegistry) -> String {
    let mut infos: Vec<IntegrationInfo> = registry
        .entries
        .values()
        .map(|entry| IntegrationInfo { connections: Vec::new(), ..entry.info.clone() })
        .collect();
    infos.sort_by(|a, b| a.name.cmp(&b.name));
    encode_json(&infos)
}
