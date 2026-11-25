use std::fmt;
use std::path::PathBuf;

use futures_util::future::BoxFuture;
use opencode_config::paths::GlobalPaths;
use opencode_location::location::LocationInfo;
use opencode_sidecar::error::SidecarError;
use opencode_sidecar::protocol::LocationRef;
use serde::{Deserialize, Serialize};

use crate::generation::{Features, Gui, Source};
use crate::operations::AddOperation;

pub trait ScriptHost: Send + Sync {
    fn resolve_local(&self, directory: PathBuf) -> BoxFuture<'static, Result<Entrypoints, HostError>>;
    fn load(&self, location: LocationRef, request: LoadRequest) -> BoxFuture<'static, Result<LoadOutcome, HostError>>;
    fn activate(
        &self,
        location: LocationRef,
        request: ActivateRequest,
    ) -> BoxFuture<'static, Result<String, HostError>>;
    fn deactivate(&self, location: LocationRef, activation: String) -> BoxFuture<'static, Result<(), HostError>>;
    fn close_location(&self, location: LocationRef) -> BoxFuture<'static, Result<(), HostError>>;
    fn npm(&self, request: NpmRequest) -> BoxFuture<'static, Result<serde_json::Value, HostError>>;
}

#[derive(Default, Clone, Debug, PartialEq, Deserialize)]
#[serde(default)]
pub struct Entrypoints {
    pub server: Option<String>,
    pub tui: Option<String>,
    pub rpc: Option<String>,
    pub gui: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadRequest {
    pub operation: AddOperation,
    pub install: bool,
    pub paths: NpmPaths,
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub enum LoadOutcome {
    Pending,
    Failed { message: String },
    Loaded(Loaded),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum LoadOutcomeWire {
    Pending(PendingWire),
    Failed(FailedWire),
    Loaded(Box<Loaded>),
}

#[derive(Deserialize)]
struct PendingWire {
    pending: bool,
}

#[derive(Deserialize)]
struct FailedWire {
    message: String,
}

impl<'de> Deserialize<'de> for LoadOutcome {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match LoadOutcomeWire::deserialize(deserializer)? {
            LoadOutcomeWire::Pending(wire) => {
                if !wire.pending {
                    return Err(serde::de::Error::custom("a load outcome marked not pending is not pending"));
                }
                Ok(LoadOutcome::Pending)
            }
            LoadOutcomeWire::Failed(wire) => Ok(LoadOutcome::Failed { message: wire.message }),
            LoadOutcomeWire::Loaded(loaded) => Ok(LoadOutcome::Loaded(*loaded)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Loaded {
    pub handle: String,
    pub id: String,
    pub features: Features,
    #[serde(default)]
    pub gui: Option<Gui>,
    pub revision: String,
    pub source: Source,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivateRequest {
    pub handle: String,
    pub options: serde_json::Map<String, serde_json::Value>,
    pub location: LocationRef,
    pub app: AppInfo,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub channel: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum NpmRequest {
    Add { pkg: String, paths: NpmPaths },
    Resolve { pkg: String, paths: NpmPaths },
    Check { pkg: String, paths: NpmPaths },
    Update { pkg: String, paths: NpmPaths },
    Follows { pkg: String, paths: NpmPaths },
    Reject { pkg: String, version: String, paths: NpmPaths },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NpmPaths {
    pub cache: PathBuf,
    pub state: PathBuf,
    pub data: PathBuf,
    pub config: PathBuf,
    pub tmp: PathBuf,
    pub bin: PathBuf,
    pub log: PathBuf,
    pub repos: PathBuf,
    pub home: PathBuf,
}

impl From<&GlobalPaths> for NpmPaths {
    fn from(paths: &GlobalPaths) -> Self {
        Self {
            cache: paths.cache.clone(),
            state: paths.state.clone(),
            data: paths.data.clone(),
            config: paths.config.clone(),
            tmp: paths.tmp.clone(),
            bin: paths.bin.clone(),
            log: paths.log.clone(),
            repos: paths.repos.clone(),
            home: paths.home.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostError {
    pub tag: String,
    pub message: String,
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.tag, self.message)
    }
}

impl std::error::Error for HostError {}

impl From<SidecarError> for HostError {
    fn from(error: SidecarError) -> Self {
        match &error {
            SidecarError::Remote(remote) => Self { tag: remote.tag.clone(), message: remote.message.clone() },
            other => Self { tag: tag_name(other).to_string(), message: error.to_string() },
        }
    }
}

fn tag_name(error: &SidecarError) -> &'static str {
    match error {
        SidecarError::BunNotFound => "BunNotFound",
        SidecarError::Spawn(_) => "Spawn",
        SidecarError::Handshake(_) => "Handshake",
        SidecarError::Exited { .. } => "Exited",
        SidecarError::Unresponsive => "Unresponsive",
        SidecarError::MemoryExceeded { .. } => "MemoryExceeded",
        SidecarError::ProtocolBroken(_) => "ProtocolBroken",
        SidecarError::TooLarge { .. } => "TooLarge",
        SidecarError::Busy => "Busy",
        SidecarError::Cancelled => "Cancelled",
        SidecarError::Remote(_) => "Remote",
        SidecarError::Unavailable { .. } => "Unavailable",
        SidecarError::Disabled { .. } => "Disabled",
        SidecarError::ShuttingDown => "ShuttingDown",
    }
}

pub fn location_ref(info: &LocationInfo) -> LocationRef {
    LocationRef { directory: info.directory.clone(), workspace_id: info.workspace_id.clone() }
}
