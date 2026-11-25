use std::sync::Arc;

use futures_util::future::BoxFuture;
use opencode_location::location::LocationInfo;
use opencode_sidecar::protocol::LocationRef;

use crate::generation::{ActivateContext, Activation, Activator, Generation};
use crate::host::{ActivateRequest, AppInfo, Loaded, ScriptHost, location_info, location_ref};

pub struct ScriptActivator {
    pub host: Arc<dyn ScriptHost>,
    pub handle: String,
    pub location: LocationInfo,
    pub app: AppInfo,
}

impl Activator for ScriptActivator {
    fn activate(&self, context: ActivateContext) -> BoxFuture<'static, Result<Box<dyn Activation>, String>> {
        let request = ActivateRequest {
            handle: self.handle.clone(),
            options: context.options,
            location: location_info(&self.location),
            app: self.app.clone(),
        };
        let location = location_ref(&self.location);
        let host = self.host.clone();
        Box::pin(async move {
            let activation = host.activate(location.clone(), request).await.map_err(|error| error.to_string())?;
            Ok(Box::new(ScriptActivation { host, location, activation }) as Box<dyn Activation>)
        })
    }
}

struct ScriptActivation {
    host: Arc<dyn ScriptHost>,
    location: LocationRef,
    activation: String,
}

impl Activation for ScriptActivation {
    fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), String>> {
        Box::pin(async move {
            self.host.deactivate(self.location, self.activation).await.map_err(|error| error.to_string())
        })
    }
}

pub(crate) fn generation(
    loaded: Loaded,
    options: serde_json::Map<String, serde_json::Value>,
    host: Arc<dyn ScriptHost>,
    location: LocationInfo,
    app: AppInfo,
) -> Generation {
    Generation {
        id: loaded.id,
        revision: loaded.revision,
        source: Some(loaded.source),
        features: Some(loaded.features),
        gui: loaded.gui,
        options,
        activator: Arc::new(ScriptActivator { host, handle: loaded.handle, location, app }),
    }
}
