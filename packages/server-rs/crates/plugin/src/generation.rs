use std::path::PathBuf;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use opencode_contract::generated::{
    PluginFeatures, PluginFeaturesGui, PluginFeaturesRpc, PluginFeaturesServer, PluginFeaturesTui, PluginID,
    PluginInfo, PluginSource, PluginSourceBuiltin, PluginSourceBuiltinType, PluginSourceLocal, PluginSourceLocalType,
    PluginSourcePackage, PluginSourcePackageOutdated, PluginSourcePackageType, PluginSourcePackageUpdating,
    PluginSourceSdk, PluginSourceSdkType, PluginStateActive, PluginStateActiveStatus, PluginStateFailed,
    PluginStateFailedStatus,
};
use opencode_location::location::LocationInfo;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Builtin,
    Package { target: String, version: Option<String>, outdated: bool, updating: bool },
    Local { path: String },
    Sdk,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Features {
    pub server: bool,
    pub tui: bool,
    pub rpc: bool,
    pub gui: bool,
}

impl Features {
    pub fn server_only() -> Features {
        Features { server: true, tui: false, rpc: false, gui: false }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Gui {
    pub file: PathBuf,
    pub revision: String,
    pub options: serde_json::Map<String, serde_json::Value>,
}

pub struct ActivateContext {
    pub location: LocationInfo,
    pub options: serde_json::Map<String, serde_json::Value>,
}

pub trait Activator: Send + Sync {
    fn activate(&self, context: ActivateContext) -> BoxFuture<'static, Result<Box<dyn Activation>, String>>;
}

pub trait Activation: Send + Sync {
    fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), String>>;
}

#[derive(Clone)]
pub struct Generation {
    pub id: String,
    pub revision: String,
    pub source: Option<Source>,
    pub features: Option<Features>,
    pub gui: Option<Gui>,
    pub options: serde_json::Map<String, serde_json::Value>,
    pub activator: Arc<dyn Activator>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    pub id: Option<String>,
    pub source: Source,
    pub features: Features,
    pub state: PluginState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginState {
    Active,
    Failed { error: String, reference: Option<String> },
}

pub type Failure = Info;

impl Info {
    pub fn to_wire(&self) -> PluginInfo {
        PluginInfo {
            id: self.id.clone().map(PluginID),
            source: match &self.source {
                Source::Builtin => {
                    PluginSource::Builtin(PluginSourceBuiltin { r#type: PluginSourceBuiltinType::Builtin })
                }
                Source::Package { target, version, outdated, updating } => PluginSource::Package(PluginSourcePackage {
                    r#type: PluginSourcePackageType::Package,
                    target: target.clone(),
                    version: version.clone(),
                    outdated: outdated.then_some(PluginSourcePackageOutdated::True),
                    updating: updating.then_some(PluginSourcePackageUpdating::True),
                }),
                Source::Local { path } => {
                    PluginSource::Local(PluginSourceLocal { r#type: PluginSourceLocalType::Local, path: path.clone() })
                }
                Source::Sdk => PluginSource::Sdk(PluginSourceSdk { r#type: PluginSourceSdkType::Sdk }),
            },
            features: PluginFeatures {
                server: self.features.server.then_some(PluginFeaturesServer::True),
                tui: self.features.tui.then_some(PluginFeaturesTui::True),
                rpc: self.features.rpc.then_some(PluginFeaturesRpc::True),
                gui: self.features.gui.then_some(PluginFeaturesGui::True),
            },
            state: match &self.state {
                PluginState::Active => opencode_contract::generated::PluginState::Active(PluginStateActive {
                    status: PluginStateActiveStatus::Active,
                }),
                PluginState::Failed { error, reference } => {
                    opencode_contract::generated::PluginState::Failed(PluginStateFailed {
                        status: PluginStateFailedStatus::Failed,
                        error: error.clone(),
                        r#ref: reference.clone(),
                    })
                }
            },
        }
    }
}
