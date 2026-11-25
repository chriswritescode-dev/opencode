use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use futures_util::future::BoxFuture;
use http::StatusCode;
use opencode_bus::{Bus, Definition, PublishOptions};
use opencode_catalog::Catalog;
use opencode_config::entry::Entry;
use opencode_config::flag;
use opencode_config::observer::{ObserverHandle, observe};
use opencode_config::options::{ConfigLocation, ConfigOptions};
use opencode_config::paths::GlobalPaths;
use opencode_config::seams::KvStore;
use opencode_config::service::{ConfigService, ServiceDeps};
use opencode_config::wellknown::WellKnown;
use opencode_contract::codec::encode_json;
use opencode_contract::generated::{
    AbsolutePath, ConfigEntryDocumentInfoExperimentalLocationIdleTimeout,
    ConfigEntryDocumentInfoExperimentalLocationIdleTimeoutBool, LocationNotFoundError, LocationNotFoundErrorTag,
    LocationPublicRef, LocationRef, ServiceUnavailableError, ServiceUnavailableErrorTag, WorkspaceID,
};
use opencode_location::activity::{Activity, ActivityOptions, IdleTimeout};
use opencode_location::environment::{Environment, local::LocalEnvironment};
use opencode_location::events::LocationEvent;
use opencode_location::lifecycle::Lifecycle;
use opencode_location::location::{BootError, LocationInfo, boot_location};
use opencode_location::map::Lease;
use opencode_location::map::LocationMap;
use opencode_location::project::{ProjectService, SWEEP_INTERVAL_MS};
use opencode_location::resolve::SessionLocationError;
use opencode_plugin::{GlobalPlugins, LocationPlugins};
use opencode_session::agent::Agents;
use opencode_session::execution::{Placed, Placement};
use opencode_session::instructions::BuiltinsInput;
use opencode_session::search_build::SessionSearchBuild;
use opencode_storage::Database;
use serde_json::Value;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;

use crate::response;

pub type ConfigEnv = Arc<dyn Fn(&str) -> Option<OsString> + Send + Sync>;

struct ConfigAiEnv(ConfigEnv);

impl opencode_ai::Env for ConfigAiEnv {
    fn var(&self, name: &str) -> Option<String> {
        (self.0)(name).map(|value| value.to_string_lossy().into_owned())
    }
}

struct LocationPlacement {
    locations: LocationMap<LocationServices>,
    store: opencode_session::SessionStore,
    paths: GlobalPaths,
    env: Arc<dyn opencode_ai::Env>,
}

struct PlacedLocation {
    _lease: Lease<LocationServices>,
    catalog: Catalog,
    agents: Agents,
    builtins: BuiltinsInput,
    env: Arc<dyn opencode_ai::Env>,
    location: LocationRef,
}

impl Placement for LocationPlacement {
    fn place(&self, session_id: String) -> BoxFuture<'static, Result<Arc<dyn Placed>, String>> {
        let locations = self.locations.clone();
        let store = self.store.clone();
        let paths = self.paths.clone();
        let env = self.env.clone();
        Box::pin(async move {
            let reference = store
                .session_location(&session_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("Session not found: {session_id}"))?;
            let lease = locations.get(&reference).await.map_err(|error| error.to_string())?;
            let entries = lease.services().config.entries();
            let catalog = Catalog::from_entries(&entries);
            let agents = Agents::from_entries(&entries, &paths.home.to_string_lossy());
            let tmp = std::fs::canonicalize(&paths.tmp).unwrap_or_else(|_| paths.tmp.clone());
            let builtins = BuiltinsInput {
                directory: lease.services().info.directory.clone(),
                workspace_root: lease.services().info.project.directory.clone(),
                vcs: lease.services().info.vcs.as_ref().map(|vcs| vcs.kind().to_owned()),
                tmp: tmp.to_string_lossy().into_owned(),
            };
            let placed = PlacedLocation {
                catalog,
                agents,
                builtins,
                env,
                location: opencode_session::execution::contract_location(&reference),
                _lease: lease,
            };
            Ok(Arc::new(placed) as Arc<dyn Placed>)
        })
    }
}

impl Placed for PlacedLocation {
    fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    fn agents(&self) -> &Agents {
        &self.agents
    }

    fn builtins(&self) -> &BuiltinsInput {
        &self.builtins
    }

    fn env(&self) -> &dyn opencode_ai::Env {
        self.env.as_ref()
    }

    fn location(&self) -> Option<&LocationRef> {
        Some(&self.location)
    }

    fn is_closed(&self) -> bool {
        self._lease.services().lifecycle.is_closed()
    }
}

pub struct GlobalServices {
    pub database: Arc<Database>,
    pub paths: GlobalPaths,
    pub projects: Arc<ProjectService>,
    pub locations: LocationMap<LocationServices>,
    pub activity: Activity<LocationServices>,
    pub wellknown: Arc<WellKnown>,
    pub search: Arc<SessionSearchBuild>,
    pub start_directory: String,
    pub environment: ConfigEnv,
    pub local_environment: Arc<dyn Environment>,
    pub sessions: Option<opencode_session::Sessions>,
    pub bus: Arc<Bus>,
    pub plugins: Arc<GlobalPlugins>,
    sweep: JoinHandle<()>,
}

pub struct LocationServices {
    pub info: LocationInfo,
    pub lifecycle: Lifecycle,
    pub environment: Arc<dyn Environment>,
    pub config: ConfigService,
    pub catalog: Arc<RwLock<Catalog>>,
    pub agents: Arc<RwLock<Agents>>,
    pub plugins: LocationPlugins,
    _config_observer: ObserverHandle,
}

impl GlobalServices {
    pub fn new(
        database: Arc<Database>,
        paths: GlobalPaths,
        start_directory: String,
        config_env: ConfigEnv,
        bus: Arc<Bus>,
        plugins: Arc<GlobalPlugins>,
    ) -> Self {
        let projects = Arc::new(ProjectService::new(database.clone()));
        let wellknown = Arc::new(WellKnown::new(Arc::new(StorageKv { database: database.clone() })));
        let environment: Arc<dyn Environment> = Arc::new(LocalEnvironment::new());
        let search = Arc::new(SessionSearchBuild::new(database.clone()));
        let channel = flag::build_channel().to_owned();
        let flag_overrides = config_env("OPENCODE_FLAGS").map(|value| value.to_string_lossy().into_owned());
        let boot_projects = projects.clone();
        let boot_environment = environment.clone();
        let boot_config_env = config_env.clone();
        let boot_paths = paths.clone();
        let boot_wellknown = wellknown.clone();
        let boot_search = search.clone();
        let boot_bus = bus.clone();
        let boot_plugins = plugins.clone();
        let locations = LocationMap::new(move |reference| {
            let projects = boot_projects.clone();
            let environment = boot_environment.clone();
            let config_env = boot_config_env.clone();
            let paths = boot_paths.clone();
            let wellknown = boot_wellknown.clone();
            let search = boot_search.clone();
            let channel = channel.clone();
            let flag_overrides = flag_overrides.clone();
            let bus = boot_bus.clone();
            let plugins = boot_plugins.clone();
            Box::pin(async move {
                let info = boot_location(&reference, &projects, environment.as_ref()).await?;
                let home = paths.home.clone();
                let config = ConfigService::start(
                    ConfigOptions::from_vars(|name| config_env(name)),
                    ConfigLocation { directory: PathBuf::from(&info.directory), project_id: info.project.id.clone() },
                    paths,
                    ServiceDeps { credentials: Arc::new(()), wellknown, channel, flag_overrides },
                )
                .await;
                let entries = config.entries();
                let catalog = Arc::new(RwLock::new(Catalog::from_entries(&entries)));
                let agents = Arc::new(RwLock::new(Agents::from_entries(&entries, &home.to_string_lossy())));
                let observer_projects = projects.clone();
                let observer_directory = info.directory.clone();
                let observer_workspace_id = info.workspace_id.clone();
                let observer_catalog = catalog.clone();
                let observer_agents = agents.clone();
                let observer_home = home.clone();
                let flags = config.clone();
                let build = search.clone();
                let _config_observer = observe(&config, move |_| {
                    if sqlite_fts_enabled(&flags) {
                        build.start();
                    }
                    let entries = flags.entries();
                    *observer_catalog.write().expect("the catalog lock is never poisoned") =
                        Catalog::from_entries(&entries);
                    *observer_agents.write().expect("the agents lock is never poisoned") =
                        Agents::from_entries(&entries, &observer_home.to_string_lossy());
                    observer_projects.publish(LocationEvent::ConfigUpdated {
                        directory: observer_directory.clone(),
                        workspace_id: observer_workspace_id.clone(),
                    });
                });
                if sqlite_fts_enabled(&config) {
                    search.start();
                }
                let reference = LocationRef {
                    directory: AbsolutePath(info.directory.clone()),
                    workspace_id: info.workspace_id.clone().map(WorkspaceID),
                };
                let publish: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                    if let Err(error) = bus.publish(
                        &Definition::ephemeral("plugin.updated"),
                        serde_json::json!({}),
                        PublishOptions { location: Some(reference.clone()), ..Default::default() },
                    ) {
                        log::warn!("failed to publish plugin.updated: {error}");
                    }
                });
                let lifecycle = Lifecycle::start(projects, &info).await?;
                let plugins = LocationPlugins::start(plugins.as_ref(), info.clone(), config.clone(), publish).await;
                Ok(Arc::new(LocationServices {
                    info,
                    lifecycle,
                    environment,
                    config,
                    catalog,
                    agents,
                    plugins,
                    _config_observer,
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
            search,
            start_directory,
            environment: config_env,
            local_environment: environment,
            sessions: None,
            bus,
            plugins,
            sweep,
        }
    }

    pub fn attach_sessions(&mut self, bus: Arc<Bus>, store: opencode_session::SessionStore, version: String) {
        let placement = Arc::new(LocationPlacement {
            locations: self.locations.clone(),
            store: store.clone(),
            paths: self.paths.clone(),
            env: Arc::new(ConfigAiEnv(self.environment.clone())),
        });
        let runner = Arc::new(opencode_session::runner::Runner::new(
            store.clone(),
            bus.clone(),
            self.database.clone(),
            placement,
            opencode_session::model_request::AppInfo {
                name: "opencode".into(),
                channel: "local".into(),
                version: version.clone(),
            },
        ));
        let execution = opencode_session::execution::Execution::new(bus.clone(), runner);
        self.sessions = Some(
            opencode_session::Sessions::new(store, bus, self.projects.clone(), self.local_environment.clone(), version)
                .with_execution(execution),
        );
    }

    pub async fn shutdown_executions(&self) {
        if let Some(sessions) = &self.sessions {
            sessions.shutdown().await;
        }
    }

    pub fn sessions(&self) -> Option<&opencode_session::Sessions> {
        self.sessions.as_ref()
    }

    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<LocationEvent> {
        self.projects.subscribe()
    }

    pub fn var(&self, name: &str) -> Option<OsString> {
        (self.environment)(name)
    }
}

pub fn bridge_location_events(services: &GlobalServices, bus: Arc<Bus>) {
    let mut events = services.subscribe_events();
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => publish_location_event(&bus, event),
                Err(RecvError::Lagged(_)) => log::warn!("location event subscriber lagged"),
                Err(RecvError::Closed) => return,
            }
        }
    });
}

fn publish_location_event(bus: &Bus, event: LocationEvent) {
    match event {
        LocationEvent::ProjectUpdated(project) => {
            let encoded = encode_json(&crate::handlers::project::contract_project(project));
            let data = serde_json::from_str(&encoded).expect("a contract event encodes as JSON");
            publish_event(bus, Definition::ephemeral("project.updated"), data, PublishOptions::default());
        }
        LocationEvent::WorktreeResolved { project_id, directory, previous, adopted } => {
            let mut data = serde_json::Map::new();
            data.insert("projectID".to_owned(), Value::String(project_id));
            data.insert("directory".to_owned(), Value::String(directory));
            data.insert("previous".to_owned(), Value::String(previous));
            if !adopted.is_empty() {
                data.insert("adopted".to_owned(), serde_json::json!(adopted));
            }
            let definition = Definition::latest_durable("worktree.resolved").expect("worktree.resolved is durable");
            publish_event(bus, definition, Value::Object(data), PublishOptions::default());
        }
        LocationEvent::LocationShutdown { directory, workspace_id } => {
            let location =
                Some(LocationRef { directory: AbsolutePath(directory), workspace_id: workspace_id.map(WorkspaceID) });
            publish_event(
                bus,
                Definition::ephemeral("location.shutdown"),
                serde_json::json!({}),
                PublishOptions { location, ..Default::default() },
            );
        }
        LocationEvent::ConfigUpdated { directory, workspace_id } => {
            let location =
                Some(LocationRef { directory: AbsolutePath(directory), workspace_id: workspace_id.map(WorkspaceID) });
            publish_event(
                bus,
                Definition::ephemeral("config.updated"),
                serde_json::json!({}),
                PublishOptions { location, ..Default::default() },
            );
        }
    }
}

fn publish_event(bus: &Bus, definition: Definition, data: Value, options: PublishOptions) {
    if let Err(error) = bus.publish(&definition, data, options) {
        log::warn!("failed to publish {}: {error}", definition.r#type);
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
            self.plugins.shutdown().await;
            self.lifecycle.shutdown().await;
        })
    }
}

pub fn public_ref(info: &LocationInfo) -> LocationPublicRef {
    LocationPublicRef { directory: AbsolutePath(info.directory.clone()) }
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

fn sqlite_fts_enabled(config: &ConfigService) -> bool {
    config.flags().into_iter().any(|(key, state)| key == "sqlite_fts" && state.enabled)
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

pub fn session_location_error(error: SessionLocationError) -> response::Response {
    match error {
        SessionLocationError::SessionNotFound(error) => {
            response::bytes(StatusCode::NOT_FOUND, "application/json", encode_json(&error))
        }
        SessionLocationError::InvalidRequest(error) => {
            response::bytes(StatusCode::BAD_REQUEST, "application/json", encode_json(&error))
        }
        SessionLocationError::Lookup(message) => {
            crate::handlers::defect::response("failed to resolve session location", message)
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
