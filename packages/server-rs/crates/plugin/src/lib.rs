pub mod generated;
pub mod generation;
pub mod host;
pub mod native;
pub mod operations;
pub mod registry;
pub mod router;
pub mod script;
pub mod sdk;
#[cfg(unix)]
pub mod sidecar;
pub mod supervisor;
pub mod update;
pub mod watch;

pub use generation::{ActivateContext, Activation, Activator, Features, Generation, Gui, Info, PluginState, Source};
pub use host::{
    ActivateRequest, AppInfo, Entrypoints, HostError, LoadOutcome, LoadRequest, Loaded, NpmPaths, NpmRequest,
    ScriptHost, location_info,
};
pub use native::{NoopActivator, builtins, guarded};
pub use operations::{AddOperation, Operation, configured, discover, parse, policy_blocks, scan, source_of};
pub use registry::{ActivateError, ActivationCell, Hold, Registry};
pub use router::{PluginRouter, RouterGuard};
pub use script::ScriptActivator;
pub use sdk::{InstancePlugins, SdkPlugins};
#[cfg(unix)]
pub use sidecar::SidecarHost;
pub use supervisor::{Supervisor, SupervisorDeps, Updates};
pub use update::{Change, INTERVAL, NpmPackage};
pub use watch::{ManualWatcher, NotifyWatcher, PluginWatcher, watch_target};
