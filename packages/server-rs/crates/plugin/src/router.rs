use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use opencode_sidecar::connection::{CallContext, Handlers, Reply, Request};
use opencode_sidecar::protocol::{LocationRef, RemoteError};
use serde_json::{Value, json};

use crate::generation::Info;
use crate::registry::{Registry, WeakRegistry};
use crate::watch::PluginWatcher;

#[derive(Clone, Default)]
pub struct PluginRouter {
    routes: Arc<Mutex<HashMap<RouteKey, Route>>>,
    next: Arc<AtomicU64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct RouteKey {
    directory: String,
    workspace_id: Option<String>,
}

impl From<LocationRef> for RouteKey {
    fn from(location: LocationRef) -> Self {
        RouteKey { directory: location.directory, workspace_id: location.workspace_id }
    }
}

#[derive(Clone)]
struct Route {
    id: u64,
    watcher: Weak<dyn PluginWatcher>,
    registry: WeakRegistry,
}

pub struct RouterGuard {
    routes: Arc<Mutex<HashMap<RouteKey, Route>>>,
    key: RouteKey,
    id: u64,
}

impl PluginRouter {
    pub fn handlers(&self, handlers: &mut Handlers) {
        let routes = self.routes.clone();
        handlers.register("plugin.watch", move |_context: CallContext, request: Request| {
            let routes = routes.clone();
            Box::pin(async move { watch(&routes, request) })
        });
        let routes = self.routes.clone();
        handlers.register("plugin.list", move |_context: CallContext, request: Request| {
            let routes = routes.clone();
            Box::pin(async move { list(&routes, request) })
        });
    }

    pub fn register(&self, location: LocationRef, watcher: Arc<dyn PluginWatcher>, registry: Registry) -> RouterGuard {
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let key = RouteKey::from(location);
        let route = Route { id, watcher: Arc::downgrade(&watcher), registry: registry.downgrade() };
        self.routes.lock().expect("the plugin router is never poisoned").insert(key.clone(), route);
        RouterGuard { routes: self.routes.clone(), key, id }
    }
}

impl RouterGuard {
    pub fn is_current(&self) -> bool {
        self.routes
            .lock()
            .expect("the plugin router is never poisoned")
            .get(&self.key)
            .is_some_and(|route| route.id == self.id)
    }
}

impl Drop for RouterGuard {
    fn drop(&mut self) {
        let mut routes = self.routes.lock().expect("the plugin router is never poisoned");
        if routes.get(&self.key).is_some_and(|route| route.id == self.id) {
            routes.remove(&self.key);
        }
    }
}

fn watch(routes: &Mutex<HashMap<RouteKey, Route>>, request: Request) -> Result<Reply, RemoteError> {
    let location = request.location.clone().ok_or_else(location_not_found)?;
    let file = request.params.get("file").and_then(Value::as_str).ok_or_else(|| RemoteError {
        tag: "PluginWatchFailed".to_string(),
        message: "plugin.watch requires a file".to_string(),
        data: None,
    })?;
    let route = lookup(routes, &location)?;
    let watcher = route.watcher.upgrade().ok_or_else(location_not_found)?;
    watcher.watch(Path::new(file)).map_err(|error| RemoteError {
        tag: "PluginWatchFailed".to_string(),
        message: error.to_string(),
        data: None,
    })?;
    Ok(Reply::new(Value::Null))
}

fn list(routes: &Mutex<HashMap<RouteKey, Route>>, request: Request) -> Result<Reply, RemoteError> {
    let location = request.location.clone().ok_or_else(location_not_found)?;
    let route = lookup(routes, &location)?;
    let registry = route.registry.upgrade().ok_or_else(location_not_found)?;
    let entries = registry.list().iter().map(Info::to_wire).collect::<Vec<_>>();
    let encoded = opencode_contract::codec::encode_json(&entries);
    let data: Value = serde_json::from_str(&encoded).map_err(|error| RemoteError {
        tag: "PluginListFailed".to_string(),
        message: error.to_string(),
        data: None,
    })?;
    Ok(Reply::new(json!({ "location": { "directory": location.directory }, "data": data })))
}

fn lookup(routes: &Mutex<HashMap<RouteKey, Route>>, location: &LocationRef) -> Result<Route, RemoteError> {
    routes
        .lock()
        .expect("the plugin router is never poisoned")
        .get(&RouteKey::from(location.clone()))
        .cloned()
        .ok_or_else(location_not_found)
}

fn location_not_found() -> RemoteError {
    RemoteError {
        tag: "PluginLocationNotFound".to_string(),
        message: "no plugin location is registered".to_string(),
        data: None,
    }
}
