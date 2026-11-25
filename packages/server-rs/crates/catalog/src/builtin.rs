pub mod config_policy;
pub mod config_provider;
pub mod models_dev;
pub mod wellknown;

use std::sync::Arc;

use opencode_bus::Bus;
use opencode_config::observer::ObserverHandle;
use opencode_config::service::ConfigService;
use opencode_config::wellknown::WellKnown;
use opencode_location::state::{ExternalBlock, Registration};
use tokio::task::JoinHandle;

use crate::credential::CredentialService;
use crate::integration::IntegrationService;
use crate::model::ModelService;
use crate::models_dev::ModelsDevService;
use crate::provider::{ProviderRegistry, ProviderService};

pub struct Services {
    pub bus: Arc<Bus>,
    pub config: ConfigService,
    pub credentials: Arc<CredentialService>,
    pub models_dev: Arc<ModelsDevService>,
    pub wellknown: Arc<WellKnown>,
    pub integration: Arc<IntegrationService>,
    pub provider: Arc<ProviderService>,
    pub model: Arc<ModelService>,
}

#[derive(Default)]
pub struct Deps {
    pub provider_plugins: Option<crate::provider_plugin::PluginContext>,
    pub external: Option<Arc<dyn ExternalBlock<ProviderRegistry>>>,
}

#[derive(Default)]
pub struct Builtin {
    pub registrations: Vec<Registration>,
    pub observers: Vec<ObserverHandle>,
    pub hooks: Vec<crate::provider_plugin::hooks::HookRegistration>,
    pub tasks: Vec<JoinHandle<()>>,
}

pub async fn register(services: Services, deps: Deps) -> Builtin {
    let mut builtin = Builtin::default();
    wellknown::register(&services, &mut builtin).await;
    models_dev::register(&services, &mut builtin).await;
    if let Some(context) = deps.provider_plugins.as_ref() {
        crate::provider_plugin::register(&services, context, &mut builtin).await;
    }
    if let Some(block) = deps.external {
        builtin.registrations.push(services.provider.block(block).await);
    }
    config_provider::register(&services, &mut builtin).await;
    config_policy::register(&services, &mut builtin).await;
    builtin
}

impl Builtin {
    pub fn record(&mut self, mut other: Builtin) {
        self.registrations.append(&mut other.registrations);
        self.observers.append(&mut other.observers);
        self.hooks.append(&mut other.hooks);
        self.tasks.append(&mut other.tasks);
    }
}

impl Drop for Builtin {
    fn drop(&mut self) {
        for task in self.tasks.drain(..) {
            task.abort();
        }
    }
}
