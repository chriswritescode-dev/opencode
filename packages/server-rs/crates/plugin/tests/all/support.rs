use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;
use opencode_config::entry::{Document, Entry, Origin};
use opencode_contract::codec::decode_json;
use opencode_contract::generated::ConfigEntryDocumentInfo;
use opencode_location::location::{LocationInfo, LocationProject};
use opencode_plugin::generation::{ActivateContext, Activation, Activator, Generation, Source};
use opencode_plugin::host::{
    ActivateRequest, Entrypoints, HostError, LoadOutcome, LoadRequest, NpmRequest, ScriptHost,
};
use opencode_sidecar::protocol::LocationRef;

pub struct RecordingActivator {
    pub id: String,
    pub revision: String,
    pub log: Arc<Mutex<Vec<String>>>,
    pub fail: bool,
    pub gate: Option<Arc<tokio::sync::Notify>>,
}

impl Activator for RecordingActivator {
    fn activate(&self, _context: ActivateContext) -> BoxFuture<'static, Result<Box<dyn Activation>, String>> {
        let id = self.id.clone();
        let revision = self.revision.clone();
        let log = self.log.clone();
        let fail = self.fail;
        let gate = self.gate.clone();
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.notified().await;
            }
            log.lock().expect("the test log is never poisoned").push(format!("activate:{id}:{revision}"));
            if fail {
                return Err(format!("{id}@{revision} failed"));
            }
            Ok(Box::new(RecordingActivation { id, revision, log }) as Box<dyn Activation>)
        })
    }
}

struct RecordingActivation {
    id: String,
    revision: String,
    log: Arc<Mutex<Vec<String>>>,
}

impl Activation for RecordingActivation {
    fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), String>> {
        let log = self.log.clone();
        let entry = format!("close:{}:{}", self.id, self.revision);
        Box::pin(async move {
            log.lock().expect("the test log is never poisoned").push(entry);
            Ok(())
        })
    }
}

pub fn generation(id: &str, revision: &str, activator: Arc<dyn Activator>) -> Generation {
    Generation {
        id: id.to_string(),
        revision: revision.to_string(),
        source: Some(Source::Builtin),
        features: None,
        gui: None,
        options: serde_json::Map::new(),
        activator,
    }
}

pub fn recorder(id: &str, revision: &str, log: &Arc<Mutex<Vec<String>>>, fail: bool) -> Arc<RecordingActivator> {
    Arc::new(RecordingActivator {
        id: id.to_string(),
        revision: revision.to_string(),
        log: log.clone(),
        fail,
        gate: None,
    })
}

pub fn location(dir: &str) -> LocationInfo {
    LocationInfo {
        directory: dir.to_string(),
        workspace_id: None,
        project: LocationProject { id: "p".to_string(), directory: dir.to_string(), canonical: dir.to_string() },
        vcs: None,
    }
}

pub fn log_entries(log: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    log.lock().expect("the test log is never poisoned").clone()
}

pub fn log_line(log: &Arc<Mutex<Vec<String>>>) -> String {
    log_entries(log).join(", ")
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "opencode-plugin-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("create the temp directory");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Default for TempDir {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub fn entries(json_documents: &[(&str, &str)], directories: &[&Path]) -> Vec<Entry> {
    json_documents
        .iter()
        .map(|(path, json)| {
            Entry::Document(Document {
                path: Some(PathBuf::from(path)),
                info: decode_json::<ConfigEntryDocumentInfo>(json)
                    .unwrap_or_else(|error| panic!("{json} failed to decode: {error}")),
                origin: Origin::File,
            })
        })
        .chain(directories.iter().map(|path| Entry::Directory { path: PathBuf::from(path) }))
        .collect()
}

pub struct FakeHost {
    resolve: Arc<Mutex<BTreeMap<PathBuf, Result<Entrypoints, HostError>>>>,
    load: Arc<Mutex<BTreeMap<String, LoadOutcome>>>,
    npm: Arc<Mutex<BTreeMap<String, serde_json::Value>>>,
    calls: Arc<Mutex<Vec<String>>>,
    gate: Option<Arc<tokio::sync::Notify>>,
}

impl FakeHost {
    pub fn new() -> Self {
        Self {
            resolve: Arc::new(Mutex::new(BTreeMap::new())),
            load: Arc::new(Mutex::new(BTreeMap::new())),
            npm: Arc::new(Mutex::new(BTreeMap::new())),
            calls: Arc::new(Mutex::new(Vec::new())),
            gate: None,
        }
    }

    pub fn resolving(self, directory: impl Into<PathBuf>, entrypoints: Entrypoints) -> Self {
        self.resolve.lock().expect("the test scripts are never poisoned").insert(directory.into(), Ok(entrypoints));
        self
    }

    #[allow(dead_code)]
    pub fn failing(self, directory: impl Into<PathBuf>, error: HostError) -> Self {
        self.resolve.lock().expect("the test scripts are never poisoned").insert(directory.into(), Err(error));
        self
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("the test log is never poisoned").clone()
    }
}

impl Default for FakeHost {
    fn default() -> Self {
        Self::new()
    }
}

impl ScriptHost for FakeHost {
    fn resolve_local(&self, directory: PathBuf) -> BoxFuture<'static, Result<Entrypoints, HostError>> {
        let resolve = self.resolve.clone();
        let calls = self.calls.clone();
        let gate = self.gate.clone();
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.notified().await;
            }
            calls
                .lock()
                .expect("the test log is never poisoned")
                .push(format!("resolve_local:{}", directory.display()));
            resolve
                .lock()
                .expect("the test scripts are never poisoned")
                .get(&directory)
                .cloned()
                .unwrap_or_else(|| Ok(Entrypoints::default()))
        })
    }

    fn load(&self, _location: LocationRef, request: LoadRequest) -> BoxFuture<'static, Result<LoadOutcome, HostError>> {
        let target = request.operation.target;
        let load = self.load.clone();
        let calls = self.calls.clone();
        let gate = self.gate.clone();
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.notified().await;
            }
            calls.lock().expect("the test log is never poisoned").push(format!("load:{target}"));
            Ok(load
                .lock()
                .expect("the test scripts are never poisoned")
                .get(&target)
                .cloned()
                .unwrap_or(LoadOutcome::Pending))
        })
    }

    fn activate(
        &self,
        _location: LocationRef,
        request: ActivateRequest,
    ) -> BoxFuture<'static, Result<String, HostError>> {
        let handle = request.handle;
        let calls = self.calls.clone();
        let gate = self.gate.clone();
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.notified().await;
            }
            calls.lock().expect("the test log is never poisoned").push(format!("activate:{handle}"));
            Ok("activation".to_string())
        })
    }

    fn deactivate(&self, _location: LocationRef, activation: String) -> BoxFuture<'static, Result<(), HostError>> {
        let calls = self.calls.clone();
        let gate = self.gate.clone();
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.notified().await;
            }
            calls.lock().expect("the test log is never poisoned").push(format!("deactivate:{activation}"));
            Ok(())
        })
    }

    fn close_location(&self, _location: LocationRef) -> BoxFuture<'static, Result<(), HostError>> {
        let calls = self.calls.clone();
        let gate = self.gate.clone();
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.notified().await;
            }
            calls.lock().expect("the test log is never poisoned").push("close_location".to_string());
            Ok(())
        })
    }

    fn npm(&self, request: NpmRequest) -> BoxFuture<'static, Result<serde_json::Value, HostError>> {
        let key = match request {
            NpmRequest::Add { pkg, .. }
            | NpmRequest::Resolve { pkg, .. }
            | NpmRequest::Check { pkg, .. }
            | NpmRequest::Update { pkg, .. }
            | NpmRequest::Follows { pkg, .. } => pkg,
            NpmRequest::Reject { pkg, version, .. } => format!("{pkg}@{version}"),
        };
        let npm = self.npm.clone();
        let calls = self.calls.clone();
        let gate = self.gate.clone();
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.notified().await;
            }
            calls.lock().expect("the test log is never poisoned").push(format!("npm:{key}"));
            Ok(npm
                .lock()
                .expect("the test scripts are never poisoned")
                .get(&key)
                .cloned()
                .unwrap_or(serde_json::Value::Null))
        })
    }
}
