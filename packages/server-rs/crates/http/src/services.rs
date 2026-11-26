use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use http::StatusCode;
use opencode_ai::HttpSender;
use opencode_ai::route::executor::ReqwestSender;
use opencode_bus::Bus;
use opencode_catalog::builtin::{self, Builtin};
use opencode_catalog::credential::CredentialService;
use opencode_catalog::integration::{CommandEnv, IntegrationService};
use opencode_catalog::model::ModelService;
use opencode_catalog::models_dev::{ModelsDevService, Options};
use opencode_catalog::provider::ProviderService;
use opencode_catalog::provider_plugin::gitlab::NoDiscovery;
use opencode_catalog::provider_plugin::hooks::ProviderHooks;
use opencode_catalog::provider_plugin::opencode::ConsoleHandle;
use opencode_catalog::provider_plugin::{AppInfo, CommandRunner, NoSessions, PluginContext, TokioCommandRunner};
use opencode_catalog::shared_cache::SharedCache;
use opencode_catalog::usage_limits::UsageLimits;
use opencode_config::entry::Entry;
use opencode_config::flag;
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
use opencode_location::map::LocationMap;
use opencode_location::project::{ProjectService, SWEEP_INTERVAL_MS};
use opencode_storage::Database;
use serde_json::Value;
use tokio::task::JoinHandle;

use crate::response;

pub type ConfigEnv = Arc<dyn Fn(&str) -> Option<OsString> + Send + Sync>;

pub struct GlobalServices {
    pub database: Arc<Database>,
    pub paths: GlobalPaths,
    pub projects: Arc<ProjectService>,
    pub locations: LocationMap<LocationServices>,
    pub activity: Activity<LocationServices>,
    pub wellknown: Arc<WellKnown>,
    pub credentials: Arc<CredentialService>,
    pub models_dev: Arc<ModelsDevService>,
    pub start_directory: String,
    pub environment: ConfigEnv,
    sweep: JoinHandle<()>,
    credentials_task: JoinHandle<()>,
}

pub struct LocationServices {
    pub info: LocationInfo,
    pub lifecycle: Lifecycle,
    pub environment: Arc<dyn Environment>,
    pub config: ConfigService,
    pub integrations: Arc<IntegrationService>,
    pub providers: Arc<ProviderService>,
    pub models: Arc<ModelService>,
    pub provider_hooks: Arc<ProviderHooks>,
    pub usage_limits: Arc<UsageLimits>,
    pub console: Arc<ConsoleHandle>,
    registrations: Mutex<Option<Builtin>>,
}

impl LocationServices {
    pub fn public_ref(&self) -> LocationPublicRef {
        LocationPublicRef { directory: AbsolutePath(self.info.directory.clone()) }
    }
}

impl GlobalServices {
    pub fn new(
        database: Arc<Database>,
        bus: Arc<Bus>,
        paths: GlobalPaths,
        start_directory: String,
        config_env: ConfigEnv,
        command_env: CommandEnv,
    ) -> Self {
        let credentials = CredentialService::new(database.clone(), bus.clone(), paths.state.join("locks"));
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
        let projects = Arc::new(ProjectService::new(database.clone()));
        let wellknown = Arc::new(WellKnown::new(Arc::new(StorageKv { database: database.clone() })));
        let environment: Arc<dyn Environment> = Arc::new(LocalEnvironment::new());
        let channel = flag::build_channel().to_owned();
        let flag_overrides = config_env("OPENCODE_FLAGS").map(|value| value.to_string_lossy().into_owned());
        let http: Arc<dyn HttpSender> = Arc::new(ReqwestSender::new());
        let http_manual_redirects: Arc<dyn HttpSender> = Arc::new(ReqwestSender::manual_redirects());
        let app = AppInfo {
            name: "opencode".to_owned(),
            version: option_env!("OPENCODE_VERSION").unwrap_or("local").to_owned(),
            channel: flag::build_channel().to_owned(),
        };
        let commands: Arc<dyn CommandRunner> = Arc::new(TokioCommandRunner);
        let shared_cache = SharedCache::new();
        let boot_projects = projects.clone();
        let boot_environment = environment.clone();
        let boot_config_env = config_env.clone();
        let boot_paths = paths.clone();
        let boot_wellknown = wellknown.clone();
        let boot_credentials = credentials.clone();
        let boot_models_dev = models_dev.clone();
        let boot_bus = bus.clone();
        let boot_command_env = command_env.clone();
        let boot_http = http.clone();
        let boot_http_manual_redirects = http_manual_redirects.clone();
        let boot_app = app.clone();
        let boot_commands = commands.clone();
        let boot_shared_cache = shared_cache.clone();
        let boot_database = database.clone();
        let locations = LocationMap::new(move |reference| {
            let projects = boot_projects.clone();
            let environment = boot_environment.clone();
            let config_env = boot_config_env.clone();
            let paths = boot_paths.clone();
            let wellknown = boot_wellknown.clone();
            let credentials = boot_credentials.clone();
            let models_dev = boot_models_dev.clone();
            let bus = boot_bus.clone();
            let command_env = boot_command_env.clone();
            let http = boot_http.clone();
            let http_manual_redirects = boot_http_manual_redirects.clone();
            let app = boot_app.clone();
            let commands = boot_commands.clone();
            let shared_cache = boot_shared_cache.clone();
            let database = boot_database.clone();
            let channel = channel.clone();
            let flag_overrides = flag_overrides.clone();
            Box::pin(async move {
                let info = boot_location(&reference, &projects, environment.as_ref()).await?;
                let config = ConfigService::start(
                    ConfigOptions::from_vars(|name| config_env(name)),
                    ConfigLocation { directory: PathBuf::from(&info.directory), project_id: info.project.id.clone() },
                    paths,
                    ServiceDeps {
                        credentials: credentials.clone(),
                        wellknown: wellknown.clone(),
                        channel,
                        flag_overrides,
                    },
                )
                .await;
                let location = LocationRef {
                    directory: AbsolutePath(info.directory.clone()),
                    workspace_id: info.workspace_id.clone().map(WorkspaceID),
                };
                let lifecycle = Lifecycle::start(projects, &info).await?;
                let integrations = IntegrationService::new(
                    credentials.clone(),
                    bus.clone(),
                    location.clone(),
                    config_env.clone(),
                    command_env,
                );
                let providers = ProviderService::new(integrations.clone(), bus.clone(), location.clone());
                let models = ModelService::new(providers.clone(), bus.clone(), location.clone(), config_env.clone());
                let usage_limits = Arc::new(UsageLimits::new(integrations.clone(), http.clone()));
                let provider_hooks = Arc::new(ProviderHooks::default());
                let console = Arc::new(ConsoleHandle::default());
                let registrations = builtin::register(
                    builtin::Services {
                        bus: bus.clone(),
                        config: config.clone(),
                        credentials: credentials.clone(),
                        models_dev,
                        wellknown,
                        integration: integrations.clone(),
                        provider: providers.clone(),
                        model: models.clone(),
                    },
                    builtin::Deps {
                        provider_plugins: Some(PluginContext {
                            http,
                            http_manual_redirects,
                            clock: Arc::new(opencode_ai::clock::SystemClock),
                            app,
                            env: config_env.clone(),
                            kv: Arc::new(StorageKv { database }),
                            commands,
                            sessions: Arc::new(NoSessions),
                            hooks: provider_hooks.clone(),
                            location,
                            shared_cache,
                            gitlab_discovery: Arc::new(NoDiscovery),
                            console: console.clone(),
                        }),
                        external: None,
                    },
                )
                .await;
                Ok(Arc::new(LocationServices {
                    info,
                    lifecycle,
                    environment,
                    config,
                    integrations,
                    providers,
                    models,
                    provider_hooks,
                    usage_limits,
                    console,
                    registrations: Mutex::new(Some(registrations)),
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
            paths,
            projects,
            locations,
            activity,
            wellknown,
            credentials,
            models_dev,
            start_directory,
            environment: config_env,
            sweep,
            credentials_task,
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
        self.credentials_task.abort();
    }
}

impl opencode_location::map::LocationServices for LocationServices {
    fn lifecycle(&self) -> &Lifecycle {
        &self.lifecycle
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            drop(self.registrations.lock().expect("lock the builtin registrations").take());
            self.integrations.shutdown();
            self.lifecycle.shutdown().await;
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

    fn scan<'a>(&'a self, prefix: &'a str, limit: usize) -> BoxFuture<'a, Vec<(String, Value)>> {
        Box::pin(async move {
            let options =
                opencode_storage::kv::ScanOptions { prefix: prefix.to_owned(), after: None, limit: Some(limit as f64) };
            match self.database.read(|connection| opencode_storage::kv::scan(connection, options)) {
                Ok(result) => result.entries.into_iter().map(|entry| (entry.key, entry.value)).collect(),
                Err(error) => {
                    log::warn!("kv scan failed for {prefix}: {error}");
                    Vec::new()
                }
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
