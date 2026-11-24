use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use http::StatusCode;
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
use opencode_location::project::ProjectService;
use opencode_storage::Database;
use serde_json::Value;

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
    pub events: tokio::sync::broadcast::Receiver<LocationEvent>,
}

pub struct LocationServices {
    pub info: LocationInfo,
    pub lifecycle: Lifecycle,
    pub environment: Arc<dyn Environment>,
    pub config: ConfigService,
}

impl GlobalServices {
    pub fn new(database: Arc<Database>, paths: GlobalPaths, start_directory: String, config_env: ConfigEnv) -> Self {
        let projects = Arc::new(ProjectService::new(database.clone()));
        let wellknown = Arc::new(WellKnown::new(Arc::new(StorageKv { database: database.clone() })));
        let environment: Arc<dyn Environment> = Arc::new(LocalEnvironment::new());
        let channel = flag::build_channel().to_owned();
        let flag_overrides = config_env("OPENCODE_FLAGS").map(|value| value.to_string_lossy().into_owned());
        let events = projects.subscribe();
        let boot_projects = projects.clone();
        let boot_environment = environment.clone();
        let boot_config_env = config_env.clone();
        let boot_paths = paths.clone();
        let boot_wellknown = wellknown.clone();
        let locations = LocationMap::new(move |reference| {
            let projects = boot_projects.clone();
            let environment = boot_environment.clone();
            let config_env = boot_config_env.clone();
            let paths = boot_paths.clone();
            let wellknown = boot_wellknown.clone();
            let channel = channel.clone();
            let flag_overrides = flag_overrides.clone();
            Box::pin(async move {
                let info = boot_location(&reference, &projects, environment.as_ref()).await?;
                let config = ConfigService::start(
                    ConfigOptions::from_vars(|name| config_env(name)),
                    ConfigLocation { directory: PathBuf::from(&info.directory), project_id: info.project.id.clone() },
                    paths,
                    ServiceDeps { credentials: Arc::new(()), wellknown, channel, flag_overrides },
                )
                .await;
                let lifecycle = Lifecycle::start(projects, &info).await?;
                Ok(Arc::new(LocationServices { info, lifecycle, environment, config }))
            })
        });
        let activity = Activity::spawn(
            locations.clone(),
            ActivityOptions {
                idle_timeout: Box::new(|services| configured_idle_timeout(&services.config)),
                ..Default::default()
            },
        );
        GlobalServices { database, paths, projects, locations, activity, wellknown, start_directory, events }
    }

    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<LocationEvent> {
        self.events.resubscribe()
    }
}

impl opencode_location::map::LocationServices for LocationServices {
    fn lifecycle(&self) -> &Lifecycle {
        &self.lifecycle
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move { self.lifecycle.shutdown().await })
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
        BootError::Project(cause) => response::json(
            StatusCode::INTERNAL_SERVER_ERROR,
            &serde_json::json!({ "_tag": "UnknownError", "message": cause.to_string() }),
        ),
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
