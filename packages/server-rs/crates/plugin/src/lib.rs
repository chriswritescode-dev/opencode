use std::sync::Arc;

use opencode_config::service::ConfigService;
use opencode_location::location::LocationInfo;

use crate::host::location_ref;

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

pub struct GlobalPlugins {
    pub host: Option<Arc<dyn ScriptHost>>,
    pub router: PluginRouter,
    pub sdk: SdkPlugins,
    pub updates: Updates,
    pub paths: NpmPaths,
    pub app: AppInfo,
}

pub struct LocationPlugins {
    supervisor: Supervisor,
    _watcher: Arc<dyn PluginWatcher>,
    _guard: RouterGuard,
}

impl LocationPlugins {
    pub async fn start(
        global: &GlobalPlugins,
        location: LocationInfo,
        config: ConfigService,
        publish: Arc<dyn Fn() + Send + Sync>,
    ) -> LocationPlugins {
        let registry = Registry::new(location.clone(), publish);
        let watcher: Arc<dyn PluginWatcher> = Arc::new(watch::NotifyWatcher::new());
        let guard = global.router.register(location_ref(&location), watcher.clone(), registry.clone());
        let supervisor = Supervisor::start(SupervisorDeps {
            registry,
            config,
            host: global.host.clone(),
            paths: global.paths.clone(),
            sdk: global.sdk.clone(),
            instance: Arc::new(InstancePlugins::empty()),
            updates: global.updates.clone(),
            location,
            app: global.app.clone(),
            watcher: Some(watcher.clone()),
            modules: watcher.changes(),
        });
        LocationPlugins { supervisor, _watcher: watcher, _guard: guard }
    }

    pub fn registry(&self) -> &Registry {
        self.supervisor.registry()
    }

    pub async fn await_activation(&self) {
        self.supervisor.registry().await_activation().await;
    }

    pub async fn shutdown(&self) {
        self.supervisor.shutdown().await;
    }
}
