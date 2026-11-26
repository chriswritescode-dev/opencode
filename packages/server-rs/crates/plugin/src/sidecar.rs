use std::path::PathBuf;

use futures_util::future::BoxFuture;
use opencode_sidecar::connection::{Reply, Request};
use opencode_sidecar::error::SidecarError;
use opencode_sidecar::protocol::LocationRef;
use opencode_sidecar::supervisor::{Sidecar, State};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::watch;

use crate::host::{ActivateRequest, Entrypoints, HostError, LoadOutcome, LoadRequest, NpmRequest, ScriptHost};

const LOAD_FAILED: &str = "Plugin failed to load";
const LOAD_ERROR_TAG: &str = "PluginModule.LoadError";

pub struct SidecarHost {
    sidecar: Sidecar,
    restarts: watch::Receiver<u64>,
}

impl SidecarHost {
    pub fn new(sidecar: Sidecar) -> SidecarHost {
        let (restarts, receiver) = watch::channel(0u64);
        let mut state = sidecar.state();
        tokio::spawn(async move {
            let mut ready = matches!(state.borrow_and_update().clone(), State::Ready { .. });
            let mut count = 0u64;
            loop {
                if state.changed().await.is_err() {
                    return;
                }
                match state.borrow_and_update().clone() {
                    State::Ready { .. } => ready = true,
                    State::Exited { .. } => {
                        if ready {
                            ready = false;
                            count += 1;
                            let _ = restarts.send_replace(count);
                        }
                    }
                    State::Idle | State::Disabled => {}
                }
            }
        });
        SidecarHost { sidecar, restarts: receiver }
    }
}

impl ScriptHost for SidecarHost {
    fn resolve_local(&self, directory: PathBuf) -> BoxFuture<'static, Result<Entrypoints, HostError>> {
        let sidecar = self.sidecar.clone();
        Box::pin(async move {
            let params = json!({ "directory": directory.to_string_lossy() });
            let reply = sidecar.call(Request::new("plugin.resolve", params)).await.map_err(HostError::from)?;
            decode(reply.value)
        })
    }

    fn load(&self, location: LocationRef, request: LoadRequest) -> BoxFuture<'static, Result<LoadOutcome, HostError>> {
        let sidecar = self.sidecar.clone();
        Box::pin(async move {
            let params = encode(&request)?;
            let request = Request::new("plugin.load", params).with_location(location);
            match sidecar.call(request).await {
                Ok(reply) => decode(reply.value),
                Err(SidecarError::Remote(remote)) if remote.tag == LOAD_ERROR_TAG => {
                    Ok(LoadOutcome::Failed { message: remote.message })
                }
                Err(error) => {
                    log::warn!("failed to load a plugin in the sidecar: {error}");
                    Ok(LoadOutcome::Failed { message: LOAD_FAILED.to_string() })
                }
            }
        })
    }

    fn activate(
        &self,
        location: LocationRef,
        request: ActivateRequest,
    ) -> BoxFuture<'static, Result<String, HostError>> {
        let sidecar = self.sidecar.clone();
        Box::pin(async move {
            let params = encode(&request)?;
            let request = Request::new("plugin.activate", params).with_location(location);
            let reply = sidecar.call(request).await.map_err(HostError::from)?;
            let activation: Activation = decode(reply.value)?;
            Ok(activation.activation)
        })
    }

    fn deactivate(&self, location: LocationRef, activation: String) -> BoxFuture<'static, Result<(), HostError>> {
        let sidecar = self.sidecar.clone();
        Box::pin(async move {
            let request =
                Request::new("plugin.deactivate", json!({ "activation": activation })).with_location(location);
            let _ = sidecar.call(request).await;
            Ok(())
        })
    }

    fn close_location(&self, location: LocationRef) -> BoxFuture<'static, Result<(), HostError>> {
        let sidecar = self.sidecar.clone();
        Box::pin(async move {
            let request = Request::new("plugin.close", json!({})).with_location(location);
            sidecar.call(request).await.map_err(HostError::from)?;
            Ok(())
        })
    }

    fn npm(&self, request: NpmRequest) -> BoxFuture<'static, Result<Value, HostError>> {
        let (method, params) = match request {
            NpmRequest::Add { pkg, paths } => ("npm.add", json!({ "pkg": pkg, "paths": paths })),
            NpmRequest::Resolve { pkg, paths } => ("npm.resolve", json!({ "pkg": pkg, "paths": paths })),
            NpmRequest::Check { pkg, paths } => ("npm.check", json!({ "pkg": pkg, "paths": paths })),
            NpmRequest::Update { pkg, paths } => ("npm.update", json!({ "pkg": pkg, "paths": paths })),
            NpmRequest::Follows { pkg, paths } => ("npm.follows", json!({ "pkg": pkg, "paths": paths })),
            NpmRequest::Reject { pkg, version, paths } => {
                ("npm.reject", json!({ "pkg": pkg, "version": version, "paths": paths }))
            }
        };
        let sidecar = self.sidecar.clone();
        Box::pin(async move {
            let reply: Reply = sidecar.call(Request::new(method, params)).await.map_err(HostError::from)?;
            Ok(reply.value)
        })
    }

    fn restarts(&self) -> Option<watch::Receiver<u64>> {
        Some(self.restarts.clone())
    }
}

#[derive(serde::Deserialize)]
struct Activation {
    activation: String,
}

fn encode<T: serde::Serialize>(value: &T) -> Result<Value, HostError> {
    serde_json::to_value(value)
        .map_err(|error| HostError { tag: "PluginProtocol".to_string(), message: error.to_string() })
}

fn decode<T: DeserializeOwned>(value: Value) -> Result<T, HostError> {
    serde_json::from_value(value)
        .map_err(|error| HostError { tag: "PluginProtocol".to_string(), message: error.to_string() })
}
