use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use opencode_config::entry::{Document, Entry, Origin};
use opencode_config::options::{ConfigLocation, ConfigOptions, SystemOption};
use opencode_config::paths::GlobalPaths;
use opencode_config::seams::NoKv;
use opencode_config::service::{ConfigService, ServiceDeps};
use opencode_config::wellknown::WellKnown;
use opencode_contract::codec::decode_json;
use opencode_contract::generated::ConfigEntryDocumentInfo;
use opencode_location::location::{LocationInfo, LocationProject};
use opencode_plugin::generation::{ActivateContext, Activation, Activator, Features, Generation, Source};
use opencode_plugin::host::{
    ActivateRequest, AppInfo, Entrypoints, HostError, LoadOutcome, LoadRequest, Loaded, NpmPaths, NpmRequest,
    ScriptHost,
};
use opencode_plugin::registry::Registry;
use opencode_plugin::sdk::{InstancePlugins, SdkPlugins};
use opencode_plugin::supervisor::{Supervisor, SupervisorDeps, Updates};
use opencode_plugin::watch::{ManualWatcher, PluginWatcher};
use opencode_sidecar::protocol::LocationRef;

pub struct RecordingActivator {
    pub id: String,
    pub revision: String,
    pub log: Arc<Mutex<Vec<String>>>,
    pub fail: bool,
    pub gate: Option<Arc<tokio::sync::Notify>>,
    pub entered: Option<Arc<tokio::sync::Notify>>,
}

impl Activator for RecordingActivator {
    fn activate(&self, _context: ActivateContext) -> BoxFuture<'static, Result<Box<dyn Activation>, String>> {
        let id = self.id.clone();
        let revision = self.revision.clone();
        let log = self.log.clone();
        let fail = self.fail;
        let gate = self.gate.clone();
        let entered = self.entered.clone();
        Box::pin(async move {
            if let Some(entered) = entered {
                entered.notify_one();
            }
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
        entered: None,
    })
}

pub fn gated(
    id: &str,
    revision: &str,
    log: &Arc<Mutex<Vec<String>>>,
    gate: Arc<tokio::sync::Notify>,
    entered: Arc<tokio::sync::Notify>,
) -> Arc<RecordingActivator> {
    Arc::new(RecordingActivator {
        id: id.to_string(),
        revision: revision.to_string(),
        log: log.clone(),
        fail: false,
        gate: Some(gate),
        entered: Some(entered),
    })
}

pub fn loaded(id: &str, revision: &str, source: Source, handle: &str) -> LoadOutcome {
    LoadOutcome::Loaded(Loaded {
        handle: handle.to_string(),
        id: id.to_string(),
        features: Features::server_only(),
        gui: None,
        revision: revision.to_string(),
        source,
    })
}

pub fn package_source(target: &str, version: &str) -> Source {
    Source::Package { target: target.to_string(), version: Some(version.to_string()), outdated: false, updating: false }
}

pub fn local_source(path: &str) -> Source {
    Source::Local { path: path.to_string() }
}

pub fn app() -> AppInfo {
    AppInfo { name: "opencode".to_string(), version: "0.0.0".to_string(), channel: "local".to_string() }
}

pub fn npm_paths() -> NpmPaths {
    NpmPaths::default()
}

pub async fn wait_for(mut predicate: impl FnMut() -> bool) {
    for _ in 0..5000 {
        if predicate() {
            return;
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("the awaited condition never became true");
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
    installed: Arc<Mutex<BTreeMap<String, LoadOutcome>>>,
    loads: Arc<Mutex<Vec<(String, bool)>>>,
    npm: Arc<Mutex<BTreeMap<String, serde_json::Value>>>,
    npm_errors: Arc<Mutex<BTreeMap<String, HostError>>>,
    npm_delay: Arc<Mutex<Duration>>,
    calls: Arc<Mutex<Vec<String>>>,
    load_gate: Arc<Mutex<Option<Arc<tokio::sync::Notify>>>>,
    npm_gate: Arc<Mutex<Option<Arc<tokio::sync::Notify>>>>,
    load_inflight: Arc<AtomicUsize>,
    load_peak: Arc<AtomicUsize>,
    npm_inflight: Arc<AtomicUsize>,
    npm_peak: Arc<AtomicUsize>,
}

impl FakeHost {
    pub fn new() -> Self {
        Self {
            resolve: Arc::new(Mutex::new(BTreeMap::new())),
            load: Arc::new(Mutex::new(BTreeMap::new())),
            installed: Arc::new(Mutex::new(BTreeMap::new())),
            loads: Arc::new(Mutex::new(Vec::new())),
            npm: Arc::new(Mutex::new(BTreeMap::new())),
            npm_errors: Arc::new(Mutex::new(BTreeMap::new())),
            npm_delay: Arc::new(Mutex::new(Duration::ZERO)),
            calls: Arc::new(Mutex::new(Vec::new())),
            load_gate: Arc::new(Mutex::new(None)),
            npm_gate: Arc::new(Mutex::new(None)),
            load_inflight: Arc::new(AtomicUsize::new(0)),
            load_peak: Arc::new(AtomicUsize::new(0)),
            npm_inflight: Arc::new(AtomicUsize::new(0)),
            npm_peak: Arc::new(AtomicUsize::new(0)),
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

    pub fn loading(self, target: impl Into<String>, outcome: LoadOutcome) -> Self {
        self.set_load(target, outcome);
        self
    }

    pub fn installing(self, target: impl Into<String>, outcome: LoadOutcome) -> Self {
        self.set_installing(target, outcome);
        self
    }

    pub fn set_load(&self, target: impl Into<String>, outcome: LoadOutcome) {
        self.load.lock().expect("the test scripts are never poisoned").insert(target.into(), outcome);
    }

    pub fn set_installing(&self, target: impl Into<String>, outcome: LoadOutcome) {
        self.installed.lock().expect("the test scripts are never poisoned").insert(target.into(), outcome);
    }

    pub fn set_npm(&self, key: &str, value: serde_json::Value) {
        self.npm.lock().expect("the test scripts are never poisoned").insert(key.to_string(), value);
    }

    pub fn set_npm_error(&self, key: &str, error: HostError) {
        self.npm_errors.lock().expect("the test scripts are never poisoned").insert(key.to_string(), error);
    }

    pub fn clear_npm_error(&self, key: &str) {
        self.npm_errors.lock().expect("the test scripts are never poisoned").remove(key);
    }

    pub fn set_npm_delay(&self, delay: Duration) {
        *self.npm_delay.lock().expect("the test scripts are never poisoned") = delay;
    }

    pub fn set_load_gate(&self, gate: Option<Arc<tokio::sync::Notify>>) {
        *self.load_gate.lock().expect("the test scripts are never poisoned") = gate;
    }

    pub fn set_npm_gate(&self, gate: Option<Arc<tokio::sync::Notify>>) {
        *self.npm_gate.lock().expect("the test scripts are never poisoned") = gate;
    }

    pub fn loads(&self) -> Vec<(String, bool)> {
        self.loads.lock().expect("the test log is never poisoned").clone()
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("the test log is never poisoned").clone()
    }

    pub fn npm_calls(&self, operation: &str) -> usize {
        self.calls
            .lock()
            .expect("the test log is never poisoned")
            .iter()
            .filter(|call| call.starts_with(&format!("npm:{operation}:")))
            .count()
    }

    pub fn load_inflight(&self) -> usize {
        self.load_inflight.load(Ordering::SeqCst)
    }

    pub fn load_peak(&self) -> usize {
        self.load_peak.load(Ordering::SeqCst)
    }

    pub fn npm_inflight(&self) -> usize {
        self.npm_inflight.load(Ordering::SeqCst)
    }

    pub fn npm_peak(&self) -> usize {
        self.npm_peak.load(Ordering::SeqCst)
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
        let gate = self.load_gate.lock().expect("the test scripts are never poisoned").clone();
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
        let install = request.install;
        let load = self.load.clone();
        let installed = self.installed.clone();
        let loads = self.loads.clone();
        let calls = self.calls.clone();
        let gate = self.load_gate.lock().expect("the test scripts are never poisoned").clone();
        let inflight = self.load_inflight.clone();
        let peak = self.load_peak.clone();
        Box::pin(async move {
            let current = inflight.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(current, Ordering::SeqCst);
            if let Some(gate) = gate {
                gate.notified().await;
            }
            calls.lock().expect("the test log is never poisoned").push(format!("load:{target}"));
            loads.lock().expect("the test log is never poisoned").push((target.clone(), install));
            let outcomes = if install { installed } else { load };
            let outcome = outcomes
                .lock()
                .expect("the test scripts are never poisoned")
                .get(&target)
                .cloned()
                .unwrap_or(LoadOutcome::Pending);
            inflight.fetch_sub(1, Ordering::SeqCst);
            Ok(outcome)
        })
    }

    fn activate(
        &self,
        _location: LocationRef,
        request: ActivateRequest,
    ) -> BoxFuture<'static, Result<String, HostError>> {
        let handle = request.handle;
        let calls = self.calls.clone();
        let gate = self.load_gate.lock().expect("the test scripts are never poisoned").clone();
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.notified().await;
            }
            calls.lock().expect("the test log is never poisoned").push(format!("activate:{handle}"));
            Ok(format!("activation:{handle}"))
        })
    }

    fn deactivate(&self, _location: LocationRef, activation: String) -> BoxFuture<'static, Result<(), HostError>> {
        let calls = self.calls.clone();
        let gate = self.load_gate.lock().expect("the test scripts are never poisoned").clone();
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
        let gate = self.load_gate.lock().expect("the test scripts are never poisoned").clone();
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.notified().await;
            }
            calls.lock().expect("the test log is never poisoned").push("close_location".to_string());
            Ok(())
        })
    }

    fn npm(&self, request: NpmRequest) -> BoxFuture<'static, Result<serde_json::Value, HostError>> {
        let key = match &request {
            NpmRequest::Add { pkg, .. } => format!("add:{pkg}"),
            NpmRequest::Resolve { pkg, .. } => format!("resolve:{pkg}"),
            NpmRequest::Check { pkg, .. } => format!("check:{pkg}"),
            NpmRequest::Update { pkg, .. } => format!("update:{pkg}"),
            NpmRequest::Follows { pkg, .. } => format!("follows:{pkg}"),
            NpmRequest::Reject { pkg, version, .. } => format!("reject:{pkg}@{version}"),
        };
        let npm = self.npm.clone();
        let errors = self.npm_errors.clone();
        let delay = *self.npm_delay.lock().expect("the test scripts are never poisoned");
        let calls = self.calls.clone();
        let gate = self.npm_gate.lock().expect("the test scripts are never poisoned").clone();
        let inflight = self.npm_inflight.clone();
        let peak = self.npm_peak.clone();
        Box::pin(async move {
            let current = inflight.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(current, Ordering::SeqCst);
            if let Some(gate) = gate {
                gate.notified().await;
            }
            calls.lock().expect("the test log is never poisoned").push(format!("npm:{key}"));
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            let error = errors.lock().expect("the test scripts are never poisoned").get(&key).cloned();
            let value = npm.lock().expect("the test scripts are never poisoned").get(&key).cloned();
            inflight.fetch_sub(1, Ordering::SeqCst);
            match error {
                Some(error) => Err(error),
                None => Ok(value.unwrap_or(serde_json::Value::Null)),
            }
        })
    }
}

pub struct ConfigWorkspace {
    _temp: TempDir,
    pub project: PathBuf,
    pub home: PathBuf,
    pub config: PathBuf,
}

impl ConfigWorkspace {
    pub fn new() -> Self {
        let temp = TempDir::new();
        let root = std::fs::canonicalize(temp.path()).expect("canonicalize the temp directory");
        let project = root.join("project");
        let home = root.join("home");
        let config = root.join("config");
        for directory in [&project, &home, &config] {
            std::fs::create_dir_all(directory).expect("create the workspace directory");
        }
        std::fs::create_dir_all(project.join(".opencode")).expect("create the project config directory");
        Self { _temp: temp, project, home, config }
    }

    pub fn write_plugins(&self, plugins: serde_json::Value) {
        self.write_config(&serde_json::json!({ "plugins": plugins }).to_string());
    }

    pub fn write_config(&self, json: &str) {
        std::fs::write(self.project.join(".opencode").join("opencode.json"), json).expect("write the project config");
    }

    pub fn plugins_dir(&self) -> PathBuf {
        let directory = self.project.join(".opencode").join("plugins");
        std::fs::create_dir_all(&directory).expect("create the discovered plugins directory");
        directory
    }

    pub async fn service(&self) -> ConfigService {
        let paths = self.paths();
        let options = ConfigOptions {
            project: Some(true),
            global: Some(false),
            system: Some(SystemOption::Disabled),
            claude: Some(false),
            file: None,
            content: None,
        };
        let location = ConfigLocation { directory: self.project.clone(), project_id: "test".to_owned() };
        let deps = ServiceDeps {
            credentials: Arc::new(()),
            wellknown: Arc::new(WellKnown::new(Arc::new(NoKv))),
            channel: "local".to_owned(),
            flag_overrides: None,
        };
        ConfigService::start(options, location, paths, deps).await
    }

    pub fn paths(&self) -> GlobalPaths {
        GlobalPaths::from_vars(|name| match name {
            "HOME" => Some(self.home.clone().into_os_string()),
            "OPENCODE_CONFIG_DIR" => Some(self.config.clone().into_os_string()),
            _ => None,
        })
        .expect("the workspace paths resolve")
    }

    pub fn npm_paths(&self) -> NpmPaths {
        NpmPaths::from(&self.paths())
    }
}

impl Default for ConfigWorkspace {
    fn default() -> Self {
        Self::new()
    }
}

pub fn supervisor(
    registry: Registry,
    config: ConfigService,
    host: Option<Arc<dyn ScriptHost>>,
    location: LocationInfo,
    sdk: SdkPlugins,
    instance: Arc<InstancePlugins>,
) -> Supervisor {
    let watcher = Arc::new(ManualWatcher::new());
    let modules = watcher.changes();
    Supervisor::start(SupervisorDeps {
        registry,
        config,
        host: host.clone(),
        paths: npm_paths(),
        sdk,
        instance,
        updates: Updates::new(host, npm_paths()),
        location,
        app: app(),
        watcher: Some(watcher as Arc<dyn PluginWatcher>),
        modules,
    })
}
