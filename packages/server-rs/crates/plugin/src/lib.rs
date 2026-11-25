pub mod generated;
pub mod generation;
pub mod native;
pub mod registry;

pub use generation::{ActivateContext, Activation, Activator, Features, Generation, Gui, Info, PluginState, Source};
pub use native::{NoopActivator, builtins, guarded};
pub use registry::{ActivateError, ActivationCell, Hold, Registry};
