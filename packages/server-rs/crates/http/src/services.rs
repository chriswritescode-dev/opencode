use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use http::StatusCode;
use opencode_bus::{Bus, Definition, PublishOptions};
use opencode_config::entry::Entry;
use opencode_config::flag;
use opencode_config::options::{ConfigLocation, ConfigOptions};
use opencode_config::paths::GlobalPaths;
use opencode_config::seams::KvStore;
use opencode_config::service::{ConfigService, ServiceDeps};
use opencode_config::wellknown::WellKnown;
use opencode_contract::codec::{Encode, encode_json};
use opencode_contract::generated::{
    AbsolutePath, ConfigEntryDocumentInfoExperimentalLocationIdleTimeout,
    ConfigEntryDocumentInfoExperimentalLocationIdleTimeoutBool, FormAnswer, FormCancelledData, FormCreatedData, FormID,
    FormInfo, FormRepliedData, LocationNotFoundError, LocationNotFoundErrorTag, LocationPublicRef, LocationRef,
    PermissionAskedData, PermissionID, PermissionRepliedData, PermissionReply, PermissionRequest, PermissionRule,
    ServiceUnavailableError, ServiceUnavailableErrorTag, SessionID, WorkspaceID,
};
use opencode_location::activity::{Activity, ActivityOptions, IdleTimeout};
use opencode_location::environment::{Environment, local::LocalEnvironment};
use opencode_location::events::LocationEvent;
use opencode_location::lifecycle::Lifecycle;
use opencode_location::location::{BootError, LocationInfo, boot_location};
use opencode_location::map::LocationMap;
use opencode_location::project::{ProjectService, SWEEP_INTERVAL_MS};
use opencode_permission::form::{FormEvents, FormService, SystemClock};
use opencode_permission::saved::PermissionSaved;
use opencode_permission::service::{
    PermissionAgents, PermissionDeps, PermissionEvents, PermissionService, PermissionSessions, SessionRules,
};
use opencode_registry::agent::AgentService;
use opencode_registry::builtin::{self, Activation};
use opencode_registry::command::CommandService;
use opencode_registry::instructions::Source;
use opencode_registry::instructions::discovery::{InstructionDiscovery, InstructionOptions};
use opencode_registry::reference::{NoRefresh, ReferenceService};
use opencode_registry::skill::SkillService;
use opencode_session::SessionStore;
use opencode_storage::Database;
use serde_json::Value;
use tokio::task::JoinHandle;

use crate::response;

pub type ConfigEnv = Arc<dyn Fn(&str) -> Option<OsString> + Send + Sync>;

type StringEnv = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

pub struct GlobalServices {
    pub database: Arc<Database>,
    pub paths: GlobalPaths,
    pub projects: Arc<ProjectService>,
    pub locations: LocationMap<LocationServices>,
    pub activity: Activity<LocationServices>,
    pub wellknown: Arc<WellKnown>,
    pub start_directory: String,
    pub environment: ConfigEnv,
    pub bus: Arc<Bus>,
    pub saved: Arc<PermissionSaved>,
    pub sessions: Arc<StoreSessionLookup>,
    sweep: JoinHandle<()>,
}

pub struct LocationServices {
    pub info: LocationInfo,
    pub lifecycle: Lifecycle,
    pub environment: Arc<dyn Environment>,
    pub config: ConfigService,
    pub agents: AgentService,
    pub commands: CommandService,
    pub skills: SkillService,
    pub references: ReferenceService,
    pub instructions: LocationInstructions,
    pub permissions: PermissionService,
    pub forms: FormService,
    pub activation: Activation,
}

pub struct LocationInstructions {
    pub discovery: InstructionDiscovery,
    pub sources: Vec<Arc<dyn Source>>,
}

impl GlobalServices {
    pub fn new(
        database: Arc<Database>,
        paths: GlobalPaths,
        start_directory: String,
        config_env: ConfigEnv,
        bus: Arc<Bus>,
        version: String,
        channel: String,
    ) -> Self {
        let projects = Arc::new(ProjectService::new(database.clone()));
        let wellknown = Arc::new(WellKnown::new(Arc::new(StorageKv { database: database.clone() })));
        let environment: Arc<dyn Environment> = Arc::new(LocalEnvironment::new());
        let saved = Arc::new(PermissionSaved::new(database.clone()));
        let sessions = Arc::new(StoreSessionLookup::new(database.clone()));
        let build_channel = flag::build_channel().to_owned();
        let flag_overrides = config_env("OPENCODE_FLAGS").map(|value| value.to_string_lossy().into_owned());
        let boot_projects = projects.clone();
        let boot_environment = environment.clone();
        let boot_config_env = config_env.clone();
        let boot_paths = paths.clone();
        let boot_wellknown = wellknown.clone();
        let boot_bus = bus.clone();
        let boot_saved = saved.clone();
        let boot_sessions = sessions.clone();
        let boot_version = version.clone();
        let boot_channel = channel.clone();
        let locations = LocationMap::new(move |reference| {
            let projects = boot_projects.clone();
            let environment = boot_environment.clone();
            let config_env = boot_config_env.clone();
            let paths = boot_paths.clone();
            let wellknown = boot_wellknown.clone();
            let bus = boot_bus.clone();
            let saved = boot_saved.clone();
            let sessions = boot_sessions.clone();
            let version = boot_version.clone();
            let channel_name = boot_channel.clone();
            let build_channel = build_channel.clone();
            let flag_overrides = flag_overrides.clone();
            Box::pin(async move {
                let info = boot_location(&reference, &projects, environment.as_ref()).await?;
                let location = LocationRef {
                    directory: AbsolutePath(info.directory.clone()),
                    workspace_id: info.workspace_id.clone().map(WorkspaceID),
                };
                let home = paths.home.clone();
                let cache = paths.cache.clone();
                let repos = paths.repos.clone();
                let config_directory = paths.config.clone();
                let tmp = paths.tmp.clone();
                let config = ConfigService::start(
                    ConfigOptions::from_vars(|name| config_env(name)),
                    ConfigLocation { directory: PathBuf::from(&info.directory), project_id: info.project.id.clone() },
                    paths,
                    ServiceDeps { credentials: Arc::new(()), wellknown, channel: build_channel, flag_overrides },
                )
                .await;
                let events = Arc::new(BusEvents { bus: bus.clone(), location: location.clone() });
                let agents = AgentService::new(events.clone());
                let commands = CommandService::new(events.clone());
                let skills = SkillService::new(events.clone());
                let references = ReferenceService::new(events.clone(), repos, Arc::new(NoRefresh));
                let string_env: StringEnv = {
                    let config_env = config_env.clone();
                    Arc::new(move |name: &str| config_env(name).map(|value| value.to_string_lossy().into_owned()))
                };
                let discovery =
                    InstructionDiscovery::new(events.clone(), InstructionOptions::from_vars(|name| string_env(name)));
                let activation = builtin::activate(builtin::Deps {
                    config: config.clone(),
                    agents: agents.clone(),
                    commands: commands.clone(),
                    skills: skills.clone(),
                    references: references.clone(),
                    instructions: discovery.clone(),
                    directory: PathBuf::from(&info.directory),
                    project_directory: PathBuf::from(&info.project.directory),
                    home: home.clone(),
                    config_directory,
                    cache,
                    app: opencode_registry::builtin::skill::AppInfo { version, channel: channel_name },
                    env: string_env,
                })
                .await;
                let sources = opencode_registry::instructions::combine([
                    opencode_registry::instructions::builtins::load(
                        &opencode_registry::instructions::builtins::Environment {
                            directory: info.directory.clone(),
                            project_directory: info.project.directory.clone(),
                            vcs: info.vcs.as_ref().map(|vcs| vcs.kind().to_string()),
                            tmp: tmp.to_string_lossy().into_owned(),
                        },
                        opencode_registry::instructions::builtins::now_millis(),
                    ),
                    vec![opencode_registry::reference::instructions::source(references.clone())],
                ])
                .expect("the built-in instruction keys are distinct");
                let permissions = PermissionService::new(PermissionDeps {
                    sessions: sessions.clone(),
                    agents: Arc::new(RegistryAgents { agents: agents.clone() }),
                    saved: saved.clone(),
                    events: events.clone(),
                    project_id: info.project.id.clone(),
                });
                let forms = FormService::new(events.clone(), Arc::new(SystemClock));
                let lifecycle = Lifecycle::start(projects, &info).await?;
                Ok(Arc::new(LocationServices {
                    info,
                    lifecycle,
                    environment,
                    config,
                    agents,
                    commands,
                    skills,
                    references,
                    instructions: LocationInstructions { discovery, sources },
                    permissions,
                    forms,
                    activation,
                }))
            })
        });
        let activity = Activity::spawn(
            locations.clone(),
            ActivityOptions {
                idle_timeout: Box::new(|services| configured_idle_timeout(&services.config)),
                ..Default::default()
            },
        );
        let sweep_projects = projects.clone();
        let sweep_environment = environment.clone();
        let mut sweep_shutdown = locations.shutdown_signal();
        let sweep = tokio::spawn(async move {
            if *sweep_shutdown.borrow() {
                return;
            }
            loop {
                if let Err(error) = sweep_projects.sweep(sweep_environment.as_ref()).await {
                    log::error!("Project archive sweep failed: {error}");
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(SWEEP_INTERVAL_MS as u64)) => {}
                    _ = sweep_shutdown.changed() => return,
                }
            }
        });
        GlobalServices {
            database,
            paths,
            projects,
            locations,
            activity,
            wellknown,
            start_directory,
            environment: config_env,
            bus,
            saved,
            sessions,
            sweep,
        }
    }

    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<LocationEvent> {
        self.projects.subscribe()
    }

    pub fn var(&self, name: &str) -> Option<OsString> {
        (self.environment)(name)
    }
}

impl Drop for GlobalServices {
    fn drop(&mut self) {
        self.sweep.abort();
    }
}

impl opencode_location::map::LocationServices for LocationServices {
    fn lifecycle(&self) -> &Lifecycle {
        &self.lifecycle
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.permissions.close();
            self.forms.close();
            self.lifecycle.shutdown().await
        })
    }
}

pub struct StoreSessionLookup {
    store: SessionStore,
}

impl StoreSessionLookup {
    pub fn new(database: Arc<Database>) -> Self {
        Self { store: SessionStore::new(database) }
    }
}

impl opencode_location::resolve::SessionLookup for StoreSessionLookup {
    fn location(&self, id: &SessionID) -> Result<Option<opencode_location::location::LocationRef>, String> {
        let Some(info) = self.store.get(&id.0).map_err(|error| error.to_string())? else {
            return Ok(None);
        };
        let workspace_id = self.store.workspace_id(&id.0).map_err(|error| error.to_string())?;
        Ok(Some(opencode_location::location::LocationRef { directory: info.location.directory.0, workspace_id }))
    }
}

impl PermissionSessions for StoreSessionLookup {
    fn get(&self, session_id: &str) -> Result<Option<SessionRules>, String> {
        let Some(info) = self.store.get(session_id).map_err(|error| error.to_string())? else {
            return Ok(None);
        };
        Ok(Some(SessionRules {
            agent: info.agent.map(|agent| agent.0),
            permissions: info.permissions.unwrap_or_default(),
        }))
    }
}

struct BusEvents {
    bus: Arc<Bus>,
    location: LocationRef,
}

impl BusEvents {
    fn publish(&self, r#type: &'static str, data: Value) {
        let options = PublishOptions { location: Some(self.location.clone()), ..Default::default() };
        if let Err(error) = self.bus.publish(&Definition::ephemeral(r#type), data, options) {
            log::warn!("failed to publish {type}: {error}");
        }
    }
}

fn encoded<T: Encode>(value: &T) -> Value {
    serde_json::from_str(&encode_json(value)).expect("a contract value encodes as JSON")
}

impl opencode_registry::events::RegistryEvents for BusEvents {
    fn updated(&self, r#type: &'static str) {
        self.publish(r#type, serde_json::json!({}));
    }
}

impl PermissionEvents for BusEvents {
    fn asked(&self, request: &PermissionRequest) {
        let data = PermissionAskedData {
            id: request.id.clone(),
            session_id: request.session_id.clone(),
            action: request.action.clone(),
            resources: request.resources.clone(),
            save: request.save.clone(),
            metadata: request.metadata.clone(),
            source: request.source.clone(),
            message: request.message.clone(),
        };
        self.publish("permission.asked", encoded(&data));
    }

    fn replied(&self, session_id: &SessionID, request_id: &PermissionID, reply: PermissionReply) {
        let data = PermissionRepliedData { session_id: session_id.clone(), request_id: request_id.clone(), reply };
        self.publish("permission.replied", encoded(&data));
    }
}

impl FormEvents for BusEvents {
    fn created(&self, form: &FormInfo) {
        self.publish("form.created", encoded(&FormCreatedData { form: form.clone() }));
    }

    fn replied(&self, id: &FormID, session_id: &str, answer: &FormAnswer) {
        let data = FormRepliedData { id: id.clone(), session_id: session_id.to_owned(), answer: answer.clone() };
        self.publish("form.replied", encoded(&data));
    }

    fn cancelled(&self, id: &FormID, session_id: &str) {
        let data = FormCancelledData { id: id.clone(), session_id: session_id.to_owned() };
        self.publish("form.cancelled", encoded(&data));
    }
}

struct RegistryAgents {
    agents: AgentService,
}

impl PermissionAgents for RegistryAgents {
    fn resolve<'a>(&'a self, id: Option<&'a str>) -> BoxFuture<'a, Option<Vec<PermissionRule>>> {
        Box::pin(async move {
            match self.agents.resolve(id).await {
                Ok(agent) => agent.map(|agent| agent.permissions),
                Err(error) => {
                    log::warn!("failed to resolve the agent for permission evaluation: {error}");
                    None
                }
            }
        })
    }
}

struct StorageKv {
    database: Arc<Database>,
}

impl KvStore for StorageKv {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Option<Value>> {
        Box::pin(async move {
            match self.database.read(|connection| opencode_storage::kv::get(connection, key)) {
                Ok(value) => value,
                Err(error) => {
                    log::warn!("kv read failed for {key}: {error}");
                    None
                }
            }
        })
    }

    fn set<'a>(&'a self, key: &'a str, value: Value) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if let Err(error) = self.database.read(|connection| opencode_storage::kv::set(connection, key, &value)) {
                log::warn!("kv write failed for {key}: {error}");
            }
        })
    }

    fn remove<'a>(&'a self, key: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if let Err(error) = self.database.read(|connection| opencode_storage::kv::remove(connection, key)) {
                log::warn!("kv delete failed for {key}: {error}");
            }
        })
    }
}

fn configured_idle_timeout(config: &ConfigService) -> Option<IdleTimeout> {
    let entries = config.entries();
    let timeout = entries.iter().rev().find_map(|entry| match entry {
        Entry::Document(document) => document.info.experimental.as_ref()?.location_idle_timeout.clone(),
        Entry::Directory { .. } => None,
    })?;
    match timeout {
        ConfigEntryDocumentInfoExperimentalLocationIdleTimeout::Bool(
            ConfigEntryDocumentInfoExperimentalLocationIdleTimeoutBool::False,
        ) => Some(IdleTimeout::Disabled),
        ConfigEntryDocumentInfoExperimentalLocationIdleTimeout::String(text) => timeout_from_string(&text),
    }
}

fn timeout_from_string(text: &str) -> Option<IdleTimeout> {
    if text == "Infinity" {
        return Some(IdleTimeout::Disabled);
    }
    let (value, unit) = text.split_once(' ')?;
    let value = value.parse::<f64>().ok()?;
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    match unit {
        "millis" => Duration::try_from_secs_f64(value / 1000.0).ok().map(IdleTimeout::After),
        "nanos" => Duration::try_from_secs_f64(value / 1_000_000_000.0).ok().map(IdleTimeout::After),
        _ => None,
    }
}

pub fn location_error(error: BootError) -> response::Response {
    match error {
        BootError::DirectoryNotFound(directory) => response::bytes(
            StatusCode::NOT_FOUND,
            "application/json",
            encode_json(&LocationNotFoundError {
                tag: LocationNotFoundErrorTag::LocationNotFoundError,
                location: LocationPublicRef { directory: AbsolutePath(directory.clone()) },
                message: format!("Location not found: {directory}"),
            }),
        ),
        error @ (BootError::Closed | BootError::Invalidated) => response::bytes(
            StatusCode::SERVICE_UNAVAILABLE,
            "application/json",
            encode_json(&ServiceUnavailableError {
                tag: ServiceUnavailableErrorTag::ServiceUnavailableError,
                message: error.to_string(),
                service: Some(Some("location".to_owned())),
            }),
        ),
        error @ (BootError::Project(_) | BootError::Panicked) => {
            crate::handlers::defect::response("failed to boot location", error)
        }
    }
}

pub fn services_unavailable() -> response::Response {
    response::bytes(
        StatusCode::SERVICE_UNAVAILABLE,
        "application/json",
        encode_json(&ServiceUnavailableError {
            tag: ServiceUnavailableErrorTag::ServiceUnavailableError,
            message: "Location services are unavailable".to_owned(),
            service: Some(Some("location".to_owned())),
        }),
    )
}
