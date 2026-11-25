use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use indexmap::IndexMap;
use opencode_config::entry::Entry;
use opencode_config::observer::is_directory_source;
use opencode_config::service::ConfigService;
use opencode_location::location::LocationInfo;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

use crate::generation::{Features, Generation, Info, PluginState, Source};
use crate::host::{AppInfo, LoadOutcome, LoadRequest, NpmPaths, ScriptHost, location_ref};
use crate::native;
use crate::operations::{self, Operation};
use crate::registry::{Hold, Registry, error_ref};
use crate::script;
use crate::sdk::{InstancePlugins, SdkPlugins};

const DEBOUNCE: Duration = Duration::from_millis(100);
const LOAD_FAILED: &str = "Plugin failed to load";

#[derive(Clone, Copy, Default)]
pub struct Updates;

pub struct SupervisorDeps {
    pub registry: Registry,
    pub config: ConfigService,
    pub host: Option<Arc<dyn ScriptHost>>,
    pub paths: NpmPaths,
    pub sdk: SdkPlugins,
    pub instance: Arc<InstancePlugins>,
    pub updates: Updates,
    pub location: LocationInfo,
    pub app: AppInfo,
    pub modules: broadcast::Receiver<()>,
}

pub struct Supervisor {
    core: Arc<Core>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

struct Core {
    registry: Registry,
    config: ConfigService,
    host: Option<Arc<dyn ScriptHost>>,
    paths: NpmPaths,
    sdk: SdkPlugins,
    instance: Arc<InstancePlugins>,
    location: LocationInfo,
    app: AppInfo,
    running: tokio::sync::Mutex<IndexMap<String, Generation>>,
    outdated: Mutex<HashSet<String>>,
    updating: Mutex<HashSet<String>>,
    script_loaded: AtomicBool,
}

type HoldSlot = Arc<Mutex<Option<Hold>>>;

#[derive(Clone, Copy)]
struct ResolveInput<'a> {
    entries: &'a [Entry],
    pre: &'a [Generation],
    post: &'a [Generation],
    operations: &'a [Operation],
    install: bool,
    running: &'a IndexMap<String, Generation>,
    host: Option<&'a Arc<dyn ScriptHost>>,
    location: &'a LocationInfo,
    paths: &'a NpmPaths,
    app: &'a AppInfo,
    script_loaded: &'a AtomicBool,
}

struct Resolved {
    plugins: Vec<Generation>,
    packages: IndexMap<String, Generation>,
    failures: Vec<Info>,
    pending: Vec<String>,
}

impl Supervisor {
    pub fn start(deps: SupervisorDeps) -> Supervisor {
        let SupervisorDeps { registry, config, host, paths, sdk, instance, location, app, modules, .. } = deps;
        let core = Arc::new(Core {
            registry: registry.clone(),
            config: config.clone(),
            host,
            paths,
            sdk: sdk.clone(),
            instance,
            location,
            app,
            running: tokio::sync::Mutex::new(IndexMap::new()),
            outdated: Mutex::new(HashSet::new()),
            updating: Mutex::new(HashSet::new()),
            script_loaded: AtomicBool::new(false),
        });
        let hold: HoldSlot = Arc::new(Mutex::new(Some(registry.hold())));
        let (observed, observed_rx) = watch::channel(0u64);
        let trigger: Arc<dyn Fn() + Send + Sync> = {
            let registry = registry.clone();
            let hold = hold.clone();
            let observed = observed.clone();
            Arc::new(move || {
                let mut slot = hold.lock().expect("the plugin supervisor hold lock is never poisoned");
                if slot.is_none() {
                    *slot = Some(registry.hold());
                }
                observed.send_modify(|value| *value += 1);
            })
        };

        let mut changes = config.changes();
        let updated = config.subscribe_updated();
        let sdk_updates = sdk.subscribe();

        let mut tasks = Vec::new();
        tasks.push({
            let config = config.clone();
            let trigger = trigger.clone();
            tokio::spawn(async move {
                loop {
                    match changes.recv().await {
                        Ok(update) => {
                            if is_directory_source(&config.entries(), &update.path, &["plugin", "plugins"]) {
                                trigger();
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => trigger(),
                        Err(broadcast::error::RecvError::Closed) => return,
                    }
                }
            })
        });
        tasks.push(spawn_trigger(updated, trigger.clone()));
        tasks.push(spawn_trigger(sdk_updates, trigger.clone()));
        tasks.push(spawn_trigger(modules, trigger));
        tasks.push({
            let core = core.clone();
            tokio::spawn(async move {
                let mut observed_rx = observed_rx;
                if let Err(error) = activate(&core).await {
                    log::error!("failed to reload plugins: {error}");
                }
                settle(&hold, &observed_rx, 0);
                loop {
                    if observed_rx.changed().await.is_err() {
                        return;
                    }
                    loop {
                        match tokio::time::timeout(DEBOUNCE, observed_rx.changed()).await {
                            Ok(Ok(())) => continue,
                            Ok(Err(_)) => return,
                            Err(_) => break,
                        }
                    }
                    let target = *observed_rx.borrow_and_update();
                    if let Err(error) = activate(&core).await {
                        log::error!("failed to reload plugins: {error}");
                    }
                    settle(&hold, &observed_rx, target);
                }
            })
        });

        Supervisor { core, tasks: Mutex::new(tasks) }
    }

    pub async fn shutdown(&self) {
        let tasks = {
            let mut tasks = self.tasks.lock().expect("the plugin supervisor task lock is never poisoned");
            std::mem::take(&mut *tasks)
        };
        for task in tasks {
            task.abort();
        }
        self.core.registry.close().await;
        if self.core.script_loaded.load(Ordering::SeqCst)
            && let Some(host) = &self.core.host
            && let Err(error) = host.close_location(location_ref(&self.core.location)).await
        {
            log::warn!("failed to close plugin location: {error}");
        }
    }

    pub fn registry(&self) -> &Registry {
        &self.core.registry
    }
}

fn settle(hold: &HoldSlot, observed: &watch::Receiver<u64>, target: u64) {
    let held = {
        let mut slot = hold.lock().expect("the plugin supervisor hold lock is never poisoned");
        if *observed.borrow() == target { slot.take() } else { None }
    };
    if let Some(held) = held {
        held.release();
    }
}

fn spawn_trigger(mut receiver: broadcast::Receiver<()>, trigger: Arc<dyn Fn() + Send + Sync>) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match receiver.recv().await {
                Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => trigger(),
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    })
}

async fn activate(core: &Core) -> Result<(), String> {
    let (builtin_pre, builtin_post) = native::builtins();
    let pre = builtin_pre.into_iter().chain(core.sdk.all()).chain(core.instance.all()).collect::<Vec<_>>();
    let post = builtin_post;
    let entries = core.config.entries();
    let operations = operations::scan(&entries, Path::new(&core.location.directory), core.host.as_deref()).await;
    let running = core.running.lock().await.clone();
    let input = ResolveInput {
        entries: &entries,
        pre: &pre,
        post: &post,
        operations: &operations,
        install: false,
        running: &running,
        host: core.host.as_ref(),
        location: &core.location,
        paths: &core.paths,
        app: &core.app,
        script_loaded: &core.script_loaded,
    };
    let immediate = resolve(input).await;
    let (outdated, updating) = core.decorations();
    apply(&core.registry, &immediate, &outdated, &updating).await?;
    if immediate.pending.is_empty() {
        *core.running.lock().await = immediate.packages;
        return Ok(());
    }
    let resolved = resolve(ResolveInput { install: true, ..input }).await;
    apply(&core.registry, &resolved, &outdated, &updating).await?;
    *core.running.lock().await = resolved.packages;
    Ok(())
}

impl Core {
    fn decorations(&self) -> (HashSet<String>, HashSet<String>) {
        (
            self.outdated.lock().expect("the plugin supervisor lock is never poisoned").clone(),
            self.updating.lock().expect("the plugin supervisor lock is never poisoned").clone(),
        )
    }
}

async fn apply(
    registry: &Registry,
    resolved: &Resolved,
    outdated: &HashSet<String>,
    updating: &HashSet<String>,
) -> Result<(), String> {
    let plugins = resolved
        .plugins
        .iter()
        .map(|plugin| {
            let mut plugin = plugin.clone();
            if let Some(source) = &plugin.source {
                plugin.source = Some(decorate(source, outdated, updating));
            }
            plugin
        })
        .collect();
    let failures = resolved
        .failures
        .iter()
        .map(|failure| Info { source: decorate(&failure.source, outdated, updating), ..failure.clone() })
        .collect();
    registry.activate(plugins, failures).await.map_err(|error| error.to_string())
}

fn decorate(source: &Source, outdated: &HashSet<String>, updating: &HashSet<String>) -> Source {
    match source {
        Source::Package { target, version, .. } => Source::Package {
            target: target.clone(),
            version: version.clone(),
            outdated: outdated.contains(target),
            updating: updating.contains(target),
        },
        other => other.clone(),
    }
}

async fn resolve(input: ResolveInput<'_>) -> Resolved {
    let definitions = input.pre.iter().chain(input.post.iter()).collect::<Vec<_>>();
    let mut enabled = definitions.iter().map(|plugin| plugin.id.clone()).collect::<HashSet<_>>();
    let mut packages: IndexMap<String, Generation> = IndexMap::new();
    let mut pending = Vec::new();
    let mut failures: IndexMap<String, Info> = IndexMap::new();

    for operation in input.operations {
        match operation {
            Operation::Remove { target } => {
                if target == "*" {
                    failures.clear();
                }
                let ids = definitions
                    .iter()
                    .map(|plugin| plugin.id.as_str())
                    .chain(packages.values().map(|plugin| plugin.id.as_str()))
                    .filter(|id| matches(target, id) && !native::guarded(id))
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                for id in ids {
                    enabled.remove(&id);
                }
            }
            Operation::Add(add) => {
                let target = add.target.as_str();
                let matched = definitions
                    .iter()
                    .map(|plugin| plugin.id.as_str())
                    .chain(packages.values().map(|plugin| plugin.id.as_str()))
                    .filter(|id| matches(target, id))
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                if !matched.is_empty() || target == "*" || target.ends_with(".*") || target.starts_with("opencode.") {
                    enabled.extend(matched);
                    continue;
                }
                if operations::policy_blocks(input.entries, target) {
                    if let Some(previous) = packages.shift_remove(target) {
                        enabled.remove(&previous.id);
                    }
                    failures.shift_remove(target);
                    continue;
                }
                let Some(host) = input.host else {
                    log::warn!("failed to load plugin: target={target} error={LOAD_FAILED}");
                    record_failure(
                        &mut failures,
                        &mut packages,
                        &mut enabled,
                        input.running,
                        target,
                        LOAD_FAILED.to_string(),
                    );
                    continue;
                };
                let request =
                    LoadRequest { operation: add.clone(), install: input.install, paths: input.paths.clone() };
                match host.load(location_ref(input.location), request).await {
                    Ok(LoadOutcome::Pending) => pending.push(target.to_string()),
                    Ok(LoadOutcome::Failed { message }) => {
                        log::warn!("failed to load plugin: target={target} error={message}");
                        record_failure(&mut failures, &mut packages, &mut enabled, input.running, target, message);
                    }
                    Ok(LoadOutcome::Loaded(loaded)) => {
                        failures.shift_remove(target);
                        if let Some(previous) = packages.get(target) {
                            enabled.remove(&previous.id);
                        }
                        input.script_loaded.store(true, Ordering::SeqCst);
                        let generation = script::generation(
                            loaded,
                            add.options.clone(),
                            host.clone(),
                            input.location.clone(),
                            input.app.clone(),
                        );
                        enabled.insert(generation.id.clone());
                        packages.insert(target.to_string(), generation);
                    }
                    Err(error) => {
                        log::warn!("failed to load plugin: target={target} error={error}");
                        record_failure(
                            &mut failures,
                            &mut packages,
                            &mut enabled,
                            input.running,
                            target,
                            LOAD_FAILED.to_string(),
                        );
                    }
                }
            }
        }
    }

    let ordered = input
        .pre
        .iter()
        .filter(|plugin| enabled.contains(&plugin.id))
        .chain(packages.values().filter(|plugin| enabled.contains(&plugin.id)))
        .chain(input.post.iter().filter(|plugin| enabled.contains(&plugin.id)))
        .collect::<Vec<_>>();
    let (plugins, duplicates) = {
        let mut seen = HashSet::new();
        let mut plugins = Vec::with_capacity(ordered.len());
        let mut duplicates = Vec::new();
        for plugin in &ordered {
            if seen.insert(plugin.id.as_str()) {
                plugins.push((*plugin).clone());
            } else {
                duplicates.push(*plugin);
            }
        }
        let duplicates = duplicates
            .into_iter()
            .map(|plugin| Info {
                id: Some(plugin.id.clone()),
                source: plugin.source.clone().unwrap_or(Source::Builtin),
                features: plugin.features.clone().unwrap_or_else(Features::server_only),
                state: PluginState::Failed { error: format!("Duplicate plugin ID: {}", plugin.id), reference: None },
            })
            .collect::<Vec<_>>();
        (plugins, duplicates)
    };

    let mut all_failures = failures.into_values().collect::<Vec<_>>();
    all_failures.extend(duplicates);
    Resolved {
        plugins,
        packages: packages.into_iter().filter(|(_, plugin)| enabled.contains(&plugin.id)).collect(),
        failures: all_failures,
        pending,
    }
}

fn matches(selector: &str, id: &str) -> bool {
    if selector == "*" {
        return true;
    }
    if selector.ends_with(".*") {
        return selector.strip_suffix('*').is_some_and(|prefix| id.starts_with(prefix));
    }
    selector == id
}

fn record_failure(
    failures: &mut IndexMap<String, Info>,
    packages: &mut IndexMap<String, Generation>,
    enabled: &mut HashSet<String>,
    running: &IndexMap<String, Generation>,
    target: &str,
    message: String,
) {
    failures.insert(
        target.to_string(),
        Info {
            id: None,
            source: operations::source_of(target, None),
            features: Features::server_only(),
            state: PluginState::Failed { error: message, reference: Some(error_ref()) },
        },
    );
    let retained = packages.get(target).or_else(|| running.get(target)).cloned();
    if let Some(retained) = retained {
        enabled.insert(retained.id.clone());
        packages.insert(target.to_string(), retained);
    }
}
