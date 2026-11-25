pub mod generated;
pub mod generation;
pub mod host;
pub mod native;
pub mod operations;
pub mod registry;

pub use generation::{ActivateContext, Activation, Activator, Features, Generation, Gui, Info, PluginState, Source};
pub use host::{
    ActivateRequest, AppInfo, Entrypoints, HostError, LoadOutcome, LoadRequest, Loaded, NpmPaths, NpmRequest,
    ScriptHost,
};
pub use native::{NoopActivator, builtins, guarded};
pub use operations::{AddOperation, Operation, configured, discover, parse, policy_blocks, scan, source_of};
pub use registry::{ActivateError, ActivationCell, Hold, Registry};
