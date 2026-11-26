use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use http::StatusCode;
use opencode_bus::{Bus, Definition, PublishOptions};
use opencode_catalog::builtin::Builtin;
use opencode_catalog::credential::CredentialService;
use opencode_catalog::integration::{CommandEnv, IntegrationService};
use opencode_catalog::model::ModelService;
use opencode_catalog::models_dev::{ModelsDevService, Options};
use opencode_catalog::provider::ProviderService;
use opencode_catalog::resolver::{NoBridge, Resolver};
use opencode_config::entry::Entry;
use opencode_config::flag;
use opencode_config::observer::{ObserverHandle, observe};
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
use opencode_location::map::Lease;
use opencode_location::map::LocationMap;
use opencode_location::project::{ProjectService, SWEEP_INTERVAL_MS};
use opencode_location::resolve::SessionLocationError;
use opencode_location::state::Registration;
use opencode_permission::form::{FormEvents, FormService, SystemClock};
use opencode_permission::saved::PermissionSaved;
use opencode_permission::service::{
    PermissionAgents, PermissionDeps, PermissionEvents, PermissionService, PermissionSessions, SessionRules,
};
use opencode_plugin::{GlobalPlugins, LocationPlugins};
use opencode_registry::agent::AgentService;
use opencode_registry::builtin::{self, Activation};
use opencode_registry::command::CommandService;
use opencode_registry::instructions::Source;
use opencode_registry::instructions::discovery::{InstructionDiscovery, InstructionOptions};
use opencode_registry::reference::{CacheRefresh, ReferenceService};
use opencode_registry::skill::SkillService;
use opencode_session::SessionStore;
use opencode_session::execution::{Placed, Placement};
use opencode_session::instructions::BuiltinsInput;
use opencode_session::search_build::SessionSearchBuild;
use opencode_storage::Database;
use opencode_vcs::git_provider::GitProvider;
use opencode_vcs::hg_provider::HgProvider;
use opencode_vcs::repository_cache::RepositoryCache;
use opencode_vcs::snapshot::Snapshot;
use opencode_vcs::vcs::Vcs;
use opencode_vcs::watcher::{LocationWatcher, LocationWatcherPolicy};
use opencode_vcs::worktree::{StrategiesLoader, Worktree, WorktreeError};
use opencode_vcs::worktree_strategies::WorktreeStrategies;
use serde_json::Value;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;

use crate::response;

pub type ConfigEnv = Arc<dyn Fn(&str) -> Option<OsString> + Send + Sync>;

type StringEnv = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

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
}

struct PlacedLocation {
    lease: Lease<LocationServices>,
    builtins: BuiltinsInput,
    location: LocationRef,
}

impl Placement for LocationPlacement {
    fn place(&self, session_id: String) -> BoxFuture<'static, Result<Arc<dyn Placed>, String>> {
        let locations = self.locations.clone();
        let store = self.store.clone();
        let paths = self.paths.clone();
        Box::pin(async move {
            let reference = store
                .session_location(&session_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("Session not found: {session_id}"))?;
            let lease = locations.get(&reference).await.map_err(|error| error.to_string())?;
            let tmp = std::fs::canonicalize(&paths.tmp).unwrap_or_else(|_| paths.tmp.clone());
            let builtins = BuiltinsInput {
                directory: lease.services().info.directory.clone(),
                workspace_root: lease.services().info.project.directory.clone(),
                vcs: lease.services().info.vcs.as_ref().map(|vcs| vcs.kind().to_owned()),
                tmp: tmp.to_string_lossy().into_owned(),
            };
            let placed = PlacedLocation {
                builtins,
                location: opencode_session::execution::contract_location(&reference),
                lease,
            };
            Ok(Arc::new(placed) as Arc<dyn Placed>)
        })
    }
}

impl Placed for PlacedLocation {
    fn models(&self) -> &ModelService {
        &self.lease.services().models
    }

    fn resolver(&self) -> &Resolver {
        &self.lease.services().resolver
    }

    fn agents(&self) -> &AgentService {
        &self.lease.services().agents
    }

    fn builtins(&self) -> &BuiltinsInput {
        &self.builtins
    }

    fn location(&self) -> Option<&LocationRef> {
        Some(&self.location)
    }

    fn is_closed(&self) -> bool {
        self.lease.services().lifecycle.is_closed()
    }
}

pub struct GlobalServices {
    pub database: Arc<Database>,
    pub bus: Arc<Bus>,
    pub paths: GlobalPaths,
    pub projects: Arc<ProjectService>,
    pub worktree: Arc<Worktree>,
    pub locations: LocationMap<LocationServices>,
    pub activity: Activity<LocationServices>,
    pub wellknown: Arc<WellKnown>,
    pub credentials: Arc<CredentialService>,
    pub models_dev: Arc<ModelsDevService>,
    pub search: Arc<SessionSearchBuild>,
    pub start_directory: String,
    pub environment: ConfigEnv,
    pub local_environment: Arc<dyn Environment>,
    pub sessions: Option<opencode_session::Sessions>,
    pub store: SessionStore,
    pub plugins: Arc<GlobalPlugins>,
    pub saved: Arc<PermissionSaved>,
    sweep: JoinHandle<()>,
    credentials_task: JoinHandle<()>,
}

pub struct LocationServices {
    pub info: LocationInfo,
    pub lifecycle: Lifecycle,
    pub environment: Arc<dyn Environment>,
    pub config: ConfigService,
    pub snapshot: Arc<Snapshot>,
    pub vcs: Arc<Vcs>,
    pub worktree_strategies: Arc<WorktreeStrategies>,
    watcher: LocationWatcher,
    _vcs_registrations: Vec<Registration>,
    _watcher_observer: ObserverHandle,
    pub agents: AgentService,
    pub commands: CommandService,
    pub skills: SkillService,
    pub references: ReferenceService,
    pub instructions: LocationInstructions,
    pub permissions: PermissionService,
    pub forms: FormService,
    pub activation: Activation,
    pub integrations: Arc<IntegrationService>,
    pub providers: Arc<ProviderService>,
    pub models: Arc<ModelService>,
    pub resolver: Arc<Resolver>,
    pub plugins: LocationPlugins,
    registrations: Mutex<Option<Builtin>>,
    _config_observer: ObserverHandle,
}

pub struct LocationInstructions {
    pub discovery: InstructionDiscovery,
    pub sources: Vec<Arc<dyn Source>>,
}

impl LocationServices {
    pub fn public_ref(&self) -> LocationPublicRef {
        LocationPublicRef { directory: AbsolutePath(self.info.directory.clone()) }
    }
}

impl GlobalServices {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        database: Arc<Database>,
        paths: GlobalPaths,
        start_directory: String,
        config_env: ConfigEnv,
        bus: Arc<Bus>,
        plugins: Arc<GlobalPlugins>,
        version: String,
        channel: String,
        command_env: CommandEnv,
    ) -> Self {
        let credentials = CredentialService::new(database.clone(), bus.clone(), paths.locks());
        let models_dev = ModelsDevService::new(
            database.clone(),
            bus.clone(),
            Options::from_vars(|name| config_env(name)),
            format!(
                "opencode/{}/{}/opencode",
                flag::build_channel(),
                option_env!("OPENCODE_VERSION").unwrap_or("local")
            ),
        );
        let git = Arc::new(opencode_git::Git::new(config_env.clone()));
        let repository_cache = Arc::new(RepositoryCache::new(
            git.clone(),
            database.clone(),
            paths.repos.clone(),
            paths.locks(),
            config_env.clone(),
        ));
        let boot_repository_cache = repository_cache.clone();
        let boot_git = git.clone();
        let projects = Arc::new(ProjectService::new(database.clone(), git));
        let wellknown = Arc::new(WellKnown::new(Arc::new(StorageKv { database: database.clone() })));
        let environment: Arc<dyn Environment> = Arc::new(LocalEnvironment::new());
        let saved = Arc::new(PermissionSaved::new(database.clone()));
        let store = SessionStore::new(database.clone());
        let search = Arc::new(SessionSearchBuild::new(database.clone()));
        let build_channel = flag::build_channel().to_owned();
        let flag_overrides = config_env("OPENCODE_FLAGS").map(|value| value.to_string_lossy().into_owned());
        let boot_projects = projects.clone();
        let boot_environment = environment.clone();
        let boot_config_env = config_env.clone();
        let boot_paths = paths.clone();
        let boot_wellknown = wellknown.clone();
        let boot_credentials = credentials.clone();
        let boot_models_dev = models_dev.clone();
        let boot_command_env = command_env.clone();
        let boot_search = search.clone();
        let boot_bus = bus.clone();
        let boot_plugins = plugins.clone();
        let boot_saved = saved.clone();
        let boot_store = store.clone();
        let boot_version = version.clone();
        let boot_channel = channel.clone();
        let locations = LocationMap::new(move |reference| {
            let projects = boot_projects.clone();
            let environment = boot_environment.clone();
            let config_env = boot_config_env.clone();
            let paths = boot_paths.clone();
            let wellknown = boot_wellknown.clone();
            let credentials = boot_credentials.clone();
            let models_dev = boot_models_dev.clone();
            let command_env = boot_command_env.clone();
            let search = boot_search.clone();
            let saved = boot_saved.clone();
            let store = boot_store.clone();
            let version = boot_version.clone();
            let channel_name = boot_channel.clone();
            let build_channel = build_channel.clone();
            let flag_overrides = flag_overrides.clone();
            let boot_git = boot_git.clone();
            let boot_repository_cache = boot_repository_cache.clone();
            let bus = boot_bus.clone();
            let plugins = boot_plugins.clone();
            Box::pin(async move {
                let info = boot_location(&reference, &projects, environment.as_ref()).await?;
                let location = LocationRef {
                    directory: AbsolutePath(info.directory.clone()),
                    workspace_id: info.workspace_id.clone().map(WorkspaceID),
                };
                let snapshot = Arc::new(Snapshot::new(boot_git.clone(), &info, paths.data.clone()));
                let worktree_strategies = Arc::new(WorktreeStrategies::new(
                    boot_git.clone(),
                    &info.project.id,
                    &info.project.canonical,
                    paths.data.clone(),
                ));
                let home = paths.home.clone();
                let cache = paths.cache.clone();
                let repos = paths.repos.clone();
                let config_directory = paths.config.clone();
                let tmp = paths.tmp.clone();
                let config = ConfigService::start(
                    ConfigOptions::from_vars(|name| config_env(name)),
                    ConfigLocation { directory: PathBuf::from(&info.directory), project_id: info.project.id.clone() },
                    paths,
                    ServiceDeps {
                        credentials: credentials.clone(),
                        wellknown: wellknown.clone(),
                        channel: build_channel,
                        flag_overrides,
                    },
                )
                .await;
                snapshot.observe_config(&config).await;
                worktree_strategies.observe_config(&config, &home).await;
                let vcs = Vcs::new(bus.clone(), &info);
                let is_hg = info.vcs.as_ref().is_some_and(|vcs| vcs.kind() == "hg");
                let mut vcs_registrations = Vec::new();
                if !is_hg {
                    let provider = Arc::new(GitProvider::new(
                        boot_git.clone(),
                        info.directory.clone(),
                        info.project.directory.clone(),
                    ));
                    vcs_registrations.push(vcs.transform(move |editor| editor.add(provider.clone())).await);
                }
                if info.vcs.is_none() || is_hg {
                    let provider = Arc::new(HgProvider::new(info.directory.clone(), info.project.directory.clone()));
                    vcs_registrations.push(vcs.transform(move |editor| editor.add(provider.clone())).await);
                }
                let policy = LocationWatcherPolicy::new();
                policy.set(watcher_ignore(&config.entries()));
                let policy_slot = policy.clone();
                let watcher_observer = observe(&config, move |entries| policy_slot.set(watcher_ignore(entries)));
                let watcher = LocationWatcher::start(boot_git, bus.clone(), &info, policy);
                let events = Arc::new(BusEvents { bus: bus.clone(), location: location.clone() });
                let agents = AgentService::new(events.clone());
                let commands = CommandService::new(events.clone());
                let skills = SkillService::new(events.clone());
                let references = ReferenceService::new(
                    events.clone(),
                    repos,
                    config_env.clone(),
                    Arc::new(CacheRefresh::new(boot_repository_cache, config_env.clone())),
                );
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
                    opencode_registry::instructions::builtins::live(
                        &opencode_registry::instructions::builtins::Environment {
                            directory: info.directory.clone(),
                            project_directory: info.project.directory.clone(),
                            vcs: info.vcs.as_ref().map(|vcs| vcs.kind().to_string()),
                            tmp: tmp.to_string_lossy().into_owned(),
                        },
                    ),
                    vec![opencode_registry::reference::instructions::source(references.clone())],
                ])
                .expect("the built-in instruction keys are distinct");
                let permissions = PermissionService::new(PermissionDeps {
                    sessions: Arc::new(StoreSessionRules { store }),
                    agents: Arc::new(RegistryAgents { agents: agents.clone() }),
                    saved: saved.clone(),
                    events: events.clone(),
                    project_id: info.project.id.clone(),
                });
                let forms = FormService::new(events.clone(), Arc::new(SystemClock));
                let integrations = IntegrationService::new(
                    credentials.clone(),
                    bus.clone(),
                    location.clone(),
                    config_env.clone(),
                    command_env,
                );
                let providers = ProviderService::new(integrations.clone(), bus.clone(), location.clone());
                let models = ModelService::new(providers.clone(), bus.clone(), location.clone(), config_env.clone());
                let registrations = opencode_catalog::builtin::register(
                    opencode_catalog::builtin::Services {
                        bus: bus.clone(),
                        config: config.clone(),
                        credentials: credentials.clone(),
                        models_dev,
                        wellknown,
                        integration: integrations.clone(),
                        provider: providers.clone(),
                        model: models.clone(),
                    },
                    opencode_catalog::builtin::Deps::default(),
                )
                .await;
                let resolver = Resolver::new(
                    providers.clone(),
                    models.clone(),
                    integrations.clone(),
                    Arc::new(ConfigAiEnv(config_env.clone())),
                    Arc::new(NoBridge),
                );
                let observer_projects = projects.clone();
                let observer_directory = info.directory.clone();
                let observer_workspace_id = info.workspace_id.clone();
                let flags = config.clone();
                let build = search.clone();
                let _config_observer = observe(&config, move |_| {
                    if sqlite_fts_enabled(&flags) {
                        build.start();
                    }
                    observer_projects.publish(LocationEvent::ConfigUpdated {
                        directory: observer_directory.clone(),
                        workspace_id: observer_workspace_id.clone(),
                    });
                });
                if sqlite_fts_enabled(&config) {
                    search.start();
                }
                let publish: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                    if let Err(error) = bus.publish(
                        &Definition::ephemeral("plugin.updated"),
                        serde_json::json!({}),
                        PublishOptions { location: Some(location.clone()), ..Default::default() },
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
                    snapshot,
                    vcs,
                    worktree_strategies,
                    watcher,
                    _vcs_registrations: vcs_registrations,
                    _watcher_observer: watcher_observer,
                    agents,
                    commands,
                    skills,
                    references,
                    instructions: LocationInstructions { discovery, sources },
                    permissions,
                    forms,
                    activation,
                    integrations,
                    providers,
                    models,
                    resolver,
                    plugins,
                    registrations: Mutex::new(Some(registrations)),
                    _config_observer,
                }))
            })
        });
        let worktree_locations = locations.clone();
        let load: StrategiesLoader = Arc::new(move |directory: String| {
            let locations = worktree_locations.clone();
            Box::pin(async move {
                let reference = opencode_location::location::LocationRef::new(directory, None);
                let lease = locations
                    .get(&reference)
                    .await
                    .map_err(|error| WorktreeError::Message(format!("failed to load worktree services: {error}")))?;
                Ok(lease.services().worktree_strategies.clone())
            })
        });
        let worktree = Arc::new(Worktree::new(database.clone(), bus.clone(), load));
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
        let mut credential_switched = bus.subscribe(&["credential.switched"], None);
        let credentials_locations = locations.clone();
        let credentials_task = tokio::spawn(async move {
            while let Some(event) = credential_switched.recv().await {
                let Some(integration_id) = event.data().get("integrationID").and_then(Value::as_str) else {
                    continue;
                };
                for reference in credentials_locations.keys().await {
                    if let Some(services) = credentials_locations.services(&reference).await {
                        services.config.credential_switched(integration_id).await;
                    }
                }
            }
        });
        GlobalServices {
            database,
            bus,
            paths,
            projects,
            worktree,
            locations,
            activity,
            wellknown,
            credentials,
            models_dev,
            search,
            start_directory,
            environment: config_env,
            local_environment: environment,
            sessions: None,
            store,
            plugins,
            saved,
            sweep,
            credentials_task,
        }
    }

    pub fn attach_sessions(&mut self, bus: Arc<Bus>, store: opencode_session::SessionStore, version: String) {
        let placement = Arc::new(LocationPlacement {
            locations: self.locations.clone(),
            store: store.clone(),
            paths: self.paths.clone(),
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
        self.credentials_task.abort();
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
            drop(self.registrations.lock().expect("lock the builtin registrations").take());
            self.integrations.shutdown();
            self.plugins.shutdown().await;
            self.watcher.shutdown();
            self.vcs.close();
            self.snapshot.close();
            self.references.close();
            self.lifecycle.shutdown().await;
        })
    }
}

fn watcher_ignore(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .flat_map(|entry| match entry {
            Entry::Document(document) => {
                document.info.watcher.as_ref().and_then(|watcher| watcher.ignore.clone()).unwrap_or_default()
            }
            Entry::Directory { .. } => Vec::new(),
        })
        .collect()
}

struct StoreSessionRules {
    store: SessionStore,
}

impl PermissionSessions for StoreSessionRules {
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

pub fn service_unavailable(service: &'static str, error: impl std::fmt::Display) -> response::Response {
    response::bytes(
        StatusCode::SERVICE_UNAVAILABLE,
        "application/json",
        encode_json(&ServiceUnavailableError {
            tag: ServiceUnavailableErrorTag::ServiceUnavailableError,
            message: error.to_string(),
            service: Some(Some(service.to_owned())),
        }),
    )
}
