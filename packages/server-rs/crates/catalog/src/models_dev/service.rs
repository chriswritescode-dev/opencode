use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use opencode_bus::{Bus, BusError, Definition, PublishOptions};
use opencode_storage::Database;
use opencode_storage::id::now_ms;
use opencode_storage::kv;
use serde_json::{Map, Value, json};

use super::normalize;
use super::{Snapshot, decode_snapshot, snapshot_text};

pub const DEFAULT_SOURCE: &str = "https://models.opencode.ai";
pub const CATALOG_PATH: &str = "/experimental/v2.0/api.json?type=chat,decision,image,video,realtime";

const REFRESH_INTERVAL: Duration = Duration::from_secs(300);
const REFRESH_TTL: f64 = 300_000.0;
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const RETRIES: usize = 2;
const RETRY_BASE_MILLIS: f64 = 200.0;

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub url: Option<String>,
    pub file: Option<PathBuf>,
    pub fetch: Option<bool>,
    pub snapshot: Option<bool>,
}

impl Options {
    pub fn from_vars(var: impl Fn(&str) -> Option<OsString>) -> Self {
        Self {
            url: var("OPENCODE_MODELS_URL").map(|value| value.to_string_lossy().into_owned()),
            file: var("OPENCODE_MODELS_PATH").filter(|value| !value.is_empty()).map(PathBuf::from),
            fetch: Some(!opencode_config::options::truthy(var("OPENCODE_DISABLE_MODELS_FETCH").as_deref())),
            snapshot: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ModelsDevError {
    #[error("models.dev request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("models.dev returned status {0}")]
    Status(u16),
    #[error("models.dev catalog is not valid JSON: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("models.dev storage failed: {0}")]
    Storage(#[from] opencode_storage::StorageError),
    #[error("models.dev event publish failed: {0}")]
    Bus(#[from] BusError),
}

pub fn cache_key(source: &str) -> String {
    if source == DEFAULT_SOURCE {
        return "models-dev:v3:catalog".to_string();
    }
    format!("models-dev:v3:catalog:{}", sha1_hex(source.as_bytes()))
}

fn sha1_hex(data: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, data)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn body_digest(text: &str) -> String {
    opencode_util::hash::sha256_hex(text.as_bytes())
}

#[derive(Default)]
struct Caches {
    generation: u64,
    slots: [Option<Vec<Snapshot>>; 2],
}

struct Fetched {
    text: String,
    etag: Option<String>,
}

struct Meta {
    updated_at: f64,
    digest: Option<String>,
    etag: Option<String>,
}

pub struct ModelsDevService {
    database: Arc<Database>,
    bus: Arc<Bus>,
    options: Options,
    source: String,
    key: String,
    meta_key: String,
    user_agent: String,
    http: reqwest::Client,
    caches: std::sync::Mutex<Caches>,
    populate_lock: tokio::sync::Mutex<()>,
    fetch_lock: tokio::sync::Mutex<()>,
    refresh_task: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl ModelsDevService {
    pub fn new(database: Arc<Database>, bus: Arc<Bus>, options: Options, user_agent: String) -> Arc<Self> {
        let source =
            options.url.as_ref().filter(|url| !url.is_empty()).cloned().unwrap_or_else(|| DEFAULT_SOURCE.to_string());
        let key = cache_key(&source);
        let meta_key = format!("{key}:meta");
        remove_legacy(&database, &key);
        let service = Arc::new(Self {
            database,
            bus,
            options,
            source,
            key,
            meta_key,
            user_agent,
            http: opencode_config::http::client(),
            caches: std::sync::Mutex::new(Caches::default()),
            populate_lock: tokio::sync::Mutex::new(()),
            fetch_lock: tokio::sync::Mutex::new(()),
            refresh_task: std::sync::Mutex::new(None),
        });
        if service.options.fetch.unwrap_or(true) {
            let weak = Arc::downgrade(&service);
            let handle = tokio::spawn(async move {
                loop {
                    let Some(service) = weak.upgrade() else { return };
                    service.refresh(false).await;
                    drop(service);
                    tokio::time::sleep(REFRESH_INTERVAL).await;
                }
            });
            *opencode_util::lock(&service.refresh_task) = Some(handle);
        }
        service
    }

    pub async fn get(&self, recommended: bool) -> Result<Vec<Snapshot>, ModelsDevError> {
        let index = usize::from(recommended);
        if let Some(snapshots) = opencode_util::lock(&self.caches).slots[index].clone() {
            return Ok(snapshots);
        }
        let _guard = self.populate_lock.lock().await;
        if let Some(snapshots) = opencode_util::lock(&self.caches).slots[index].clone() {
            return Ok(snapshots);
        }
        let generation = opencode_util::lock(&self.caches).generation;
        let snapshots = self.populate(recommended).await?;
        let mut caches = opencode_util::lock(&self.caches);
        if caches.generation == generation {
            caches.slots[index] = Some(snapshots.clone());
        }
        Ok(snapshots)
    }

    pub async fn refresh(&self, force: bool) {
        if let Err(error) = self.refresh_inner(force).await {
            log::error!("Failed to fetch models.dev: {error}");
        }
    }

    async fn populate(&self, recommended: bool) -> Result<Vec<Snapshot>, ModelsDevError> {
        if let Some(file) = &self.options.file
            && let Some(catalog) = load_file(file)
        {
            return Ok(normalize::normalize(&catalog, recommended));
        }
        if self.options.file.is_none()
            && let Some(catalog) = self.load_from_cache()?
        {
            return Ok(normalize::normalize(&catalog, recommended));
        }
        if self.options.snapshot != Some(false)
            && let Some(bundled) = bundled(recommended)
        {
            return Ok(bundled);
        }
        if !self.options.fetch.unwrap_or(true) {
            return Ok(Vec::new());
        }
        let _guard = self.fetch_lock.lock().await;
        let cached = if self.options.file.is_some() { None } else { self.load_from_cache()? };
        let catalog = match cached {
            Some(catalog) => catalog,
            None => self.fetch_and_write().await?,
        };
        Ok(normalize::normalize(&catalog, recommended))
    }

    async fn refresh_inner(&self, force: bool) -> Result<(), ModelsDevError> {
        let _guard = self.fetch_lock.lock().await;
        let stored = match self.read_value(&self.meta_key)? {
            Some(value) => decode_meta(&value),
            None => self.read_value(&self.key)?.as_ref().and_then(decode_meta),
        };
        let now = now_ms() as f64;
        if !force && stored.as_ref().is_some_and(|stored| now - stored.updated_at < REFRESH_TTL) {
            return Ok(());
        }
        let etag = (!force).then(|| stored.as_ref().and_then(|stored| stored.etag.clone())).flatten();
        let response = self.fetch_api(etag.as_deref()).await?;
        let digest = response.as_ref().map(|response| body_digest(&response.text));
        if !force
            && let Some(stored) = &stored
            && (response.is_none() || stored.digest == digest)
        {
            let etag = response.as_ref().and_then(|response| response.etag.clone()).or_else(|| stored.etag.clone());
            self.write_meta(now, stored.digest.as_deref(), etag.as_deref())?;
            return Ok(());
        }
        let Some(response) = response else {
            return Ok(());
        };
        decode_catalog(&response.text)?;
        self.write_cache(&response.text, response.etag.as_deref(), digest.as_deref());
        {
            let mut caches = opencode_util::lock(&self.caches);
            caches.slots = [None, None];
            caches.generation += 1;
        }
        self.bus.publish(
            &Definition::ephemeral("models-dev.refreshed"),
            json!({}),
            PublishOptions { global: true, ..Default::default() },
        )?;
        Ok(())
    }

    fn load_from_cache(&self) -> Result<Option<Map<String, Value>>, ModelsDevError> {
        let Some(value) = self.read_value(&self.key)? else {
            return Ok(None);
        };
        let Some(catalog) = decode_cache(&value) else {
            self.database.read(|connection| {
                kv::remove(connection, &self.key)?;
                kv::remove(connection, &self.meta_key)
            })?;
            return Ok(None);
        };
        Ok(Some(catalog))
    }

    fn read_value(&self, key: &str) -> Result<Option<Value>, ModelsDevError> {
        Ok(self.database.read(|connection| kv::get(connection, key))?)
    }

    fn write_cache(&self, text: &str, etag: Option<&str>, digest: Option<&str>) {
        let updated_at = now_ms() as f64;
        let body = opencode_util::json_object! {
            "updatedAt": updated_at,
            "digest"?: digest,
            "body": text,
        };
        let meta = opencode_util::json_object! {
            "updatedAt": updated_at,
            "digest"?: digest,
            "etag"?: etag,
        };
        let result = self.database.read(|connection| {
            kv::set(connection, &self.key, &body)?;
            kv::set(connection, &self.meta_key, &meta)
        });
        if let Err(error) = result {
            log::warn!("Failed to cache models.dev catalog: {error}");
        }
    }

    fn write_meta(&self, updated_at: f64, digest: Option<&str>, etag: Option<&str>) -> Result<(), ModelsDevError> {
        let meta = opencode_util::json_object! {
            "updatedAt": updated_at,
            "digest"?: digest,
            "etag"?: etag,
        };
        self.database.read(|connection| kv::set(connection, &self.meta_key, &meta))?;
        Ok(())
    }

    async fn fetch_api(&self, etag: Option<&str>) -> Result<Option<Fetched>, ModelsDevError> {
        let response = self.fetch_path(CATALOG_PATH, etag).await;
        if self.source != DEFAULT_SOURCE && matches!(&response, Err(ModelsDevError::Status(404))) {
            return self.fetch_path("/api.json", etag).await;
        }
        response
    }

    async fn fetch_path(&self, path: &str, etag: Option<&str>) -> Result<Option<Fetched>, ModelsDevError> {
        let url = format!("{}{}", self.source, path);
        let mut attempt = 0;
        loop {
            let mut request =
                self.http.get(&url).header(reqwest::header::USER_AGENT, &self.user_agent).timeout(FETCH_TIMEOUT);
            if let Some(etag) = etag {
                request = request.header(reqwest::header::IF_NONE_MATCH, etag);
            }
            let result = send(request, etag.is_some()).await;
            if attempt == RETRIES || !retryable(&result) {
                return result;
            }
            let factor = 0.8 + rand::random::<f64>() * 0.4;
            let delay = (RETRY_BASE_MILLIS * (1u32 << attempt) as f64 * factor).round() as u64;
            tokio::time::sleep(Duration::from_millis(delay)).await;
            attempt += 1;
        }
    }

    async fn fetch_and_write(&self) -> Result<Map<String, Value>, ModelsDevError> {
        let Some(response) = self.fetch_api(None).await? else {
            return Ok(Map::new());
        };
        let catalog = decode_catalog(&response.text)?;
        self.write_cache(&response.text, response.etag.as_deref(), Some(&body_digest(&response.text)));
        Ok(catalog)
    }
}

impl Drop for ModelsDevService {
    fn drop(&mut self) {
        if let Some(handle) = opencode_util::lock(&self.refresh_task).take() {
            handle.abort();
        }
    }
}

fn remove_legacy(database: &Database, key: &str) {
    for prefix in ["models-dev:", "models-dev:v2:"] {
        let old = key.replacen("models-dev:v3:", prefix, 1);
        let result = database.read(|connection| {
            kv::remove(connection, &old)?;
            kv::remove(connection, &format!("{old}:meta"))
        });
        if let Err(error) = result {
            log::warn!("Failed to remove legacy models.dev cache {old}: {error}");
        }
    }
}

fn load_file(path: &Path) -> Option<Map<String, Value>> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn decode_cache(value: &Value) -> Option<Map<String, Value>> {
    let cache = value.as_object()?;
    cache.get("updatedAt")?.as_f64()?;
    string_field(cache, "digest")?;
    serde_json::from_str(cache.get("body")?.as_str()?).ok()
}

fn decode_meta(value: &Value) -> Option<Meta> {
    let meta = value.as_object()?;
    let updated_at = meta.get("updatedAt")?.as_f64()?;
    Some(Meta { updated_at, digest: string_field(meta, "digest")?, etag: string_field(meta, "etag")? })
}

fn string_field(meta: &Map<String, Value>, key: &str) -> Option<Option<String>> {
    match meta.get(key) {
        Some(value) => Some(Some(value.as_str()?.to_string())),
        None => Some(None),
    }
}

fn decode_catalog(text: &str) -> Result<Map<String, Value>, ModelsDevError> {
    Ok(serde_json::from_str(text)?)
}

async fn send(request: reqwest::RequestBuilder, conditional: bool) -> Result<Option<Fetched>, ModelsDevError> {
    let response = request.send().await?;
    if conditional && response.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(ModelsDevError::Status(response.status().as_u16()));
    }
    let etag = response.headers().get(reqwest::header::ETAG).and_then(|value| value.to_str().ok()).map(str::to_string);
    let text = response.text().await?;
    Ok(Some(Fetched { text, etag }))
}

fn retryable(result: &Result<Option<Fetched>, ModelsDevError>) -> bool {
    match result {
        Err(ModelsDevError::Request(_)) => true,
        Err(ModelsDevError::Status(status)) => matches!(*status, 408 | 429 | 500 | 502 | 503 | 504),
        _ => false,
    }
}

fn bundled(recommended: bool) -> Option<Vec<Snapshot>> {
    static BUNDLED: LazyLock<std::sync::Mutex<[Option<Vec<Snapshot>>; 2]>> =
        LazyLock::new(|| std::sync::Mutex::new([None, None]));
    let index = usize::from(recommended);
    let mut bundled = opencode_util::lock(&BUNDLED);
    if bundled[index].is_none() {
        bundled[index] = decode_snapshot(snapshot_text(), recommended).ok();
    }
    bundled[index].clone()
}
