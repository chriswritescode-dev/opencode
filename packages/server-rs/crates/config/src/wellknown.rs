use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::sync::{Semaphore, broadcast};

use crate::seams::{BoxFuture, KvStore, WellKnownEntry, WellKnownError, WellKnownSource};

const SOURCES_KEY: &str = "wellknown:sources";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const UPDATES_CAPACITY: usize = 16;

pub struct WellKnown {
    kv: Arc<dyn KvStore>,
    http: reqwest::Client,
    cache: Mutex<Vec<WellKnownEntry>>,
    lock: Semaphore,
    updates: broadcast::Sender<()>,
}

impl WellKnown {
    pub fn new(kv: Arc<dyn KvStore>) -> Self {
        Self {
            kv,
            http: crate::http::client(),
            cache: Mutex::new(Vec::new()),
            lock: Semaphore::new(1),
            updates: broadcast::channel(UPDATES_CAPACITY).0,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.updates.subscribe()
    }

    pub async fn inspect(&self, origin: &str) -> Result<Value, WellKnownError> {
        let url = format!("{}/.well-known/opencode", origin.trim_end_matches('/'));
        let response = self
            .http
            .get(&url)
            .header(reqwest::header::ACCEPT, "application/json")
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|_| failure("manifest", &url))?;
        if !response.status().is_success() {
            return Err(failure("manifest", &url));
        }
        let body: Value = response.json().await.map_err(|_| failure("manifest", &url))?;
        manifest(&body).ok_or_else(|| failure("manifest", &url))
    }

    pub async fn entries(&self) -> Vec<WellKnownEntry> {
        let origins = self.read_origins().await;
        let current = self.snapshot();
        let mut next: Vec<WellKnownEntry> = Vec::new();
        for origin in origins {
            if next.iter().any(|entry| entry.origin == origin) {
                continue;
            }
            let entry = match current.iter().find(|entry| entry.origin == origin) {
                Some(entry) => Some(entry.clone()),
                None => self.load_source(&origin).await,
            };
            if let Some(entry) = entry {
                next.push(entry);
            }
        }
        *self.cache.lock().expect("the wellknown cache lock is not poisoned") = next.clone();
        next
    }

    pub fn snapshot(&self) -> Vec<WellKnownEntry> {
        self.cache.lock().expect("the wellknown cache lock is not poisoned").clone()
    }

    pub async fn refresh(&self) -> bool {
        let _permit = self.lock.acquire().await.expect("the wellknown operation lock is never closed");
        let origins = self.read_origins().await;
        if origins.is_empty() {
            return false;
        }
        let mut next: Vec<WellKnownEntry> = Vec::new();
        for origin in origins {
            if next.iter().any(|entry| entry.origin == origin) {
                continue;
            }
            if let Some(entry) = self.load_source(&origin).await {
                next.push(entry);
            }
        }
        if equal(&self.snapshot(), &next) {
            return false;
        }
        *self.cache.lock().expect("the wellknown cache lock is not poisoned") = next;
        let _ = self.updates.send(());
        true
    }

    pub async fn add(&self, origin: &str) -> Result<WellKnownEntry, WellKnownError> {
        let origin = origin.trim_end_matches('/');
        let _permit = self.lock.acquire().await.expect("the wellknown operation lock is never closed");
        let entry = self.load_entry(origin).await?;
        if entry.manifest.get("auth").is_none() {
            return Err(WellKnownError::from(std::io::Error::other(format!(
                "No authentication method found at {origin}"
            ))));
        }
        let mut origins = self.read_origins().await;
        if !origins.iter().any(|item| item == origin) {
            origins.push(origin.to_owned());
        }
        self.kv.set(SOURCES_KEY, origins_value(origins)).await;
        self.store_entry(entry.clone());
        let _ = self.updates.send(());
        Ok(entry)
    }

    pub async fn remove(&self, origin: &str) {
        let origin = origin.trim_end_matches('/');
        let _permit = self.lock.acquire().await.expect("the wellknown operation lock is never closed");
        let origins = self.read_origins().await;
        let remaining = origins.into_iter().filter(|item| item != origin).collect();
        self.kv.set(SOURCES_KEY, origins_value(remaining)).await;
        self.kv.remove(&manifest_key(origin)).await;
        self.kv.remove(&config_key(origin)).await;
        self.cache.lock().expect("the wellknown cache lock is not poisoned").retain(|entry| entry.origin != origin);
        let _ = self.updates.send(());
    }

    pub async fn resolve(
        &self,
        entry: &WellKnownEntry,
        variables: &BTreeMap<String, String>,
    ) -> Result<Vec<Value>, WellKnownError> {
        let mut configs = Vec::new();
        if let Some(config) = entry.manifest.get("config").filter(|config| config.is_object()) {
            configs.push(config.clone());
        }
        let Some(remote) = entry.manifest.get("remote_config").filter(|remote| remote.is_object()) else {
            self.store_config(&entry.origin, &configs).await;
            return Ok(configs);
        };
        let url = substitute(remote.get("url").and_then(Value::as_str).unwrap_or_default(), variables);
        let mut request =
            self.http.get(&url).header(reqwest::header::ACCEPT, "application/json").timeout(REQUEST_TIMEOUT);
        if let Some(headers) = remote.get("headers").and_then(Value::as_object) {
            for (name, value) in headers {
                request = request.header(name.as_str(), substitute(value.as_str().unwrap_or_default(), variables));
            }
        }
        let response = request.send().await.map_err(|_| failure("remote config", &url))?;
        if !response.status().is_success() {
            return Err(failure("remote config", &url));
        }
        let body: Value = response.json().await.map_err(|_| failure("remote config", &url))?;
        let remote_config = match body.get("config").filter(|config| config.is_object()) {
            Some(config) => config.clone(),
            None if body.is_object() => body,
            None => return Err(failure("remote config", &url)),
        };
        configs.push(remote_config);
        self.store_config(&entry.origin, &configs).await;
        Ok(configs)
    }

    pub async fn cached(&self, origin: &str) -> Vec<Value> {
        let Some(Value::Array(values)) = self.kv.get(&config_key(origin)).await else {
            return Vec::new();
        };
        if !values.iter().all(Value::is_object) {
            return Vec::new();
        }
        values
    }

    pub async fn clear_cached(&self, origin: &str) {
        self.kv.remove(&config_key(origin)).await;
    }

    async fn load_entry(&self, origin: &str) -> Result<WellKnownEntry, WellKnownError> {
        let value = match self.inspect(origin).await {
            Ok(value) => {
                self.kv.set(&manifest_key(origin), value.clone()).await;
                value
            }
            Err(error) => match self.kv.get(&manifest_key(origin)).await {
                Some(cached) if manifest(&cached).is_some() => {
                    log::warn!("failed to load wellknown manifest origin={origin} error={error}");
                    cached
                }
                _ => return Err(error),
            },
        };
        Ok(WellKnownEntry { origin: origin.to_owned(), integration_id: origin.to_owned(), manifest: value })
    }

    async fn load_source(&self, origin: &str) -> Option<WellKnownEntry> {
        match self.load_entry(origin).await {
            Ok(entry) => Some(entry),
            Err(error) => {
                log::warn!("skipping wellknown source origin={origin} error={error}");
                None
            }
        }
    }

    async fn read_origins(&self) -> Vec<String> {
        let Some(Value::Array(values)) = self.kv.get(SOURCES_KEY).await else {
            return Vec::new();
        };
        if !values.iter().all(Value::is_string) {
            return Vec::new();
        }
        values.into_iter().filter_map(|value| value.as_str().map(str::to_owned)).collect()
    }

    async fn store_config(&self, origin: &str, configs: &[Value]) {
        self.kv.set(&config_key(origin), Value::Array(configs.to_vec())).await;
    }

    fn store_entry(&self, entry: WellKnownEntry) {
        let mut cache = self.cache.lock().expect("the wellknown cache lock is not poisoned");
        if let Some(existing) = cache.iter_mut().find(|current| current.origin == entry.origin) {
            *existing = entry;
            return;
        }
        cache.push(entry);
    }
}

impl WellKnownSource for WellKnown {
    fn entries<'a>(&'a self) -> BoxFuture<'a, Vec<WellKnownEntry>> {
        Box::pin(async move { WellKnown::entries(self).await })
    }

    fn resolve<'a>(
        &'a self,
        entry: &'a WellKnownEntry,
        variables: &'a BTreeMap<String, String>,
    ) -> BoxFuture<'a, Result<Vec<Value>, WellKnownError>> {
        Box::pin(async move { WellKnown::resolve(self, entry, variables).await })
    }

    fn cached<'a>(&'a self, origin: &'a str) -> BoxFuture<'a, Vec<Value>> {
        Box::pin(async move { WellKnown::cached(self, origin).await })
    }

    fn clear_cached<'a>(&'a self, origin: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move { WellKnown::clear_cached(self, origin).await })
    }
}

fn manifest(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    let mut normalized = Map::new();
    if let Some(auth) = object.get("auth") {
        normalized.insert("auth".to_owned(), auth_value(auth)?);
    }
    if let Some(config) = object.get("config") {
        if !config.is_null() && !config.is_object() {
            return None;
        }
        normalized.insert("config".to_owned(), config.clone());
    }
    if let Some(remote) = object.get("remote_config") {
        normalized.insert("remote_config".to_owned(), remote_config_value(remote)?);
    }
    Some(Value::Object(normalized))
}

fn auth_value(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    let command = object.get("command")?.as_array()?;
    if !command.iter().all(Value::is_string) {
        return None;
    }
    let env = object.get("env")?.as_str()?;
    Some(json!({ "command": command, "env": env }))
}

fn remote_config_value(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    let url = object.get("url")?.as_str()?;
    let mut normalized = Map::new();
    normalized.insert("url".to_owned(), Value::String(url.to_owned()));
    if let Some(headers) = object.get("headers") {
        let headers = headers.as_object()?;
        if !headers.values().all(Value::is_string) {
            return None;
        }
        normalized.insert("headers".to_owned(), Value::Object(headers.clone()));
    }
    Some(Value::Object(normalized))
}

fn substitute(value: &str, variables: &BTreeMap<String, String>) -> String {
    let mut result = String::new();
    let mut rest = value;
    while let Some(start) = rest.find("{env:") {
        let after = start + "{env:".len();
        let Some(end) = rest[after..].find('}') else {
            break;
        };
        if end == 0 {
            result.push_str(&rest[..after + 1]);
            rest = &rest[after + 1..];
            continue;
        }
        result.push_str(&rest[..start]);
        let name = &rest[after..after + end];
        if let Some(value) = variables.get(name).cloned().or_else(|| std::env::var(name).ok()) {
            result.push_str(&value);
        }
        rest = &rest[after + end + 1..];
    }
    result.push_str(rest);
    result
}

fn equal(left: &[WellKnownEntry], right: &[WellKnownEntry]) -> bool {
    left.len() == right.len() && left.iter().all(|entry| right.contains(entry))
}

fn origins_value(origins: Vec<String>) -> Value {
    Value::Array(origins.into_iter().map(Value::String).collect())
}

fn manifest_key(origin: &str) -> String {
    format!("wellknown:manifest:{origin}")
}

fn config_key(origin: &str) -> String {
    format!("wellknown:config:{origin}")
}

fn failure(kind: &str, url: &str) -> WellKnownError {
    WellKnownError::from(std::io::Error::other(format!("Failed to load wellknown {kind} from {url}")))
}
