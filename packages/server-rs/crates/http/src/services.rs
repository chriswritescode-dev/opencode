use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use http::StatusCode;
use opencode_bus::Bus;
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
    LocationPublicRef, ServiceUnavailableError, ServiceUnavailableErrorTag,
};
use opencode_location::activity::{Activity, ActivityOptions, IdleTimeout};
use opencode_location::environment::{Environment, local::LocalEnvironment};
use opencode_location::events::LocationEvent;
use opencode_location::lifecycle::Lifecycle;
use opencode_location::location::{BootError, LocationInfo, boot_location};
use opencode_location::map::LocationMap;
use opencode_location::project::{ProjectService, SWEEP_INTERVAL_MS};
use opencode_location::state::Registration;
use opencode_storage::Database;
use opencode_vcs::git_provider::GitProvider;
use opencode_vcs::snapshot::Snapshot;
use opencode_vcs::vcs::Vcs;
use opencode_vcs::watcher::{LocationWatcher, LocationWatcherPolicy};
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
    pub start_directory: String,
    pub environment: ConfigEnv,
    sweep: JoinHandle<()>,
}

pub struct LocationServices {
    pub info: LocationInfo,
    pub lifecycle: Lifecycle,
    pub environment: Arc<dyn Environment>,
    pub config: ConfigService,
    pub snapshot: Arc<Snapshot>,
    pub vcs: Arc<Vcs>,
    watcher: LocationWatcher,
    _vcs_registration: Option<Registration>,
    _watcher_observer: opencode_config::observer::ObserverHandle,
}

impl GlobalServices {
    pub fn new(
        database: Arc<Database>,
        paths: GlobalPaths,
        start_directory: String,
        config_env: ConfigEnv,
        bus: Arc<Bus>,
    ) -> Self {
        let git = Arc::new(opencode_git::Git::new(config_env.clone()));
        let boot_git = git.clone();
        let projects = Arc::new(ProjectService::new(database.clone(), git));
        let wellknown = Arc::new(WellKnown::new(Arc::new(StorageKv { database: database.clone() })));
        let environment: Arc<dyn Environment> = Arc::new(LocalEnvironment::new());
        let channel = flag::build_channel().to_owned();
        let flag_overrides = config_env("OPENCODE_FLAGS").map(|value| value.to_string_lossy().into_owned());
        let boot_projects = projects.clone();
        let boot_environment = environment.clone();
        let boot_config_env = config_env.clone();
        let boot_paths = paths.clone();
        let boot_wellknown = wellknown.clone();
        let boot_bus = bus.clone();
        let locations = LocationMap::new(move |reference| {
            let projects = boot_projects.clone();
            let environment = boot_environment.clone();
            let config_env = boot_config_env.clone();
            let paths = boot_paths.clone();
            let wellknown = boot_wellknown.clone();
            let channel = channel.clone();
            let flag_overrides = flag_overrides.clone();
            let boot_git = boot_git.clone();
            let bus = boot_bus.clone();
            Box::pin(async move {
                let info = boot_location(&reference, &projects, environment.as_ref()).await?;
                let snapshot = Arc::new(Snapshot::new(boot_git.clone(), &info, paths.data.clone()));
                let config = ConfigService::start(
                    ConfigOptions::from_vars(|name| config_env(name)),
                    ConfigLocation { directory: PathBuf::from(&info.directory), project_id: info.project.id.clone() },
                    paths,
                    ServiceDeps { credentials: Arc::new(()), wellknown, channel, flag_overrides },
                )
                .await;
                snapshot.observe_config(&config).await;
                let vcs = Vcs::new(bus.clone(), &info);
                let vcs_registration = if matches!(info.vcs, Some(opencode_location::project::Vcs::Hg { .. })) {
                    None
                } else {
                    let provider = Arc::new(GitProvider::new(
                        boot_git.clone(),
                        info.directory.clone(),
                        info.project.directory.clone(),
                    ));
                    let registration = vcs.transform(move |editor| editor.add(provider.clone())).await;
                    Some(registration)
                };
                let policy = LocationWatcherPolicy::new();
                policy.set(watcher_ignore(&config.entries()));
                let policy_slot = policy.clone();
                let watcher_observer = opencode_config::observer::observe(&config, move |entries| {
                    policy_slot.set(watcher_ignore(entries))
                });
                let watcher = LocationWatcher::start(boot_git, bus, &info, policy);
                let lifecycle = Lifecycle::start(projects, &info).await?;
                Ok(Arc::new(LocationServices {
                    info,
                    lifecycle,
                    environment,
                    config,
                    snapshot,
                    vcs,
                    watcher,
                    _vcs_registration: vcs_registration,
                    _watcher_observer: watcher_observer,
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
            self.watcher.shutdown();
            self.vcs.close();
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
