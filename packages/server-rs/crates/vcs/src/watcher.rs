use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use notify::{RecursiveMode, Watcher};
use opencode_bus::{Bus, Definition, PublishOptions};
use opencode_contract::generated::{AbsolutePath, LocationRef, WorkspaceID};
use opencode_git::Git;
use opencode_location::location::LocationInfo;
use opencode_location::project::Vcs;
use opencode_util::path::realpath;
use serde_json::json;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::sync::watch;
use tokio::task::JoinHandle;

#[derive(Clone)]
pub struct LocationWatcherPolicy {
    inner: Arc<PolicyInner>,
}

struct PolicyInner {
    ignore: Mutex<Vec<String>>,
    version: watch::Sender<u64>,
}

impl Default for LocationWatcherPolicy {
    fn default() -> Self {
        LocationWatcherPolicy {
            inner: Arc::new(PolicyInner { ignore: Mutex::new(Vec::new()), version: watch::channel(0).0 }),
        }
    }
}

impl LocationWatcherPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn current(&self) -> Vec<String> {
        self.inner.ignore.lock().unwrap_or_else(|error| error.into_inner()).clone()
    }

    pub fn add(&self, ignore: &[String]) {
        self.inner.ignore.lock().unwrap_or_else(|error| error.into_inner()).extend_from_slice(ignore);
        self.bump();
    }

    pub fn set(&self, ignore: Vec<String>) {
        *self.inner.ignore.lock().unwrap_or_else(|error| error.into_inner()) = ignore;
        self.bump();
    }

    fn bump(&self) {
        self.inner.version.send_modify(|version| *version = version.wrapping_add(1));
    }

    fn subscribe(&self) -> watch::Receiver<u64> {
        self.inner.version.subscribe()
    }
}

pub struct LocationWatcher {
    task: JoinHandle<()>,
}

impl LocationWatcher {
    pub fn start(git: Arc<Git>, bus: Arc<Bus>, info: &LocationInfo, policy: LocationWatcherPolicy) -> Self {
        let info = info.clone();
        LocationWatcher { task: tokio::spawn(run(git, bus, info, policy)) }
    }

    pub fn shutdown(&self) {
        self.task.abort();
    }
}

impl Drop for LocationWatcher {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Target {
    path: PathBuf,
    aliases: Vec<String>,
}

struct Watch {
    path: PathBuf,
    _watcher: notify::RecommendedWatcher,
}

struct WatchEvent {
    path: PathBuf,
    kind: &'static str,
}

async fn run(git: Arc<Git>, bus: Arc<Bus>, info: LocationInfo, policy: LocationWatcherPolicy) {
    let target = resolve(&git, &info).await;
    let location = location_ref(&info);
    let (sender, mut events) = unbounded_channel();
    let mut version = policy.subscribe();
    let mut active: Option<Watch> = None;
    loop {
        reconcile(&mut active, target.as_ref(), &policy, &sender);
        let stopped = tokio::select! {
            changed = version.changed() => changed.is_err(),
            Some(event) = events.recv() => {
                if active.as_ref().is_some_and(|watch| watch.path == event.path) {
                    publish(&bus, &location, &event);
                }
                false
            }
        };
        if stopped {
            return;
        }
    }
}

fn reconcile(
    active: &mut Option<Watch>,
    target: Option<&Target>,
    policy: &LocationWatcherPolicy,
    sender: &UnboundedSender<WatchEvent>,
) {
    let ignore = policy.current();
    let desired = target.filter(|target| !is_ignored(target, &ignore));
    if active.as_ref().is_some_and(|watch| desired.is_some_and(|desired| watch.path == desired.path)) {
        return;
    }
    *active = None;
    let Some(desired) = desired else { return };
    *active = start(desired, sender.clone());
}

fn is_ignored(target: &Target, ignore: &[String]) -> bool {
    target.aliases.iter().any(|alias| ignore.contains(alias))
}

fn start(target: &Target, sender: UnboundedSender<WatchEvent>) -> Option<Watch> {
    let parent = target.path.parent()?.to_path_buf();
    let path = target.path.clone();
    let watched = path.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else { return };
        let Some(kind) = event_kind(event.kind) else { return };
        if !event.paths.iter().any(|candidate| candidate == &watched) {
            return;
        }
        let _ = sender.send(WatchEvent { path: watched.clone(), kind });
    })
    .inspect_err(|error| log::warn!("failed to create the location watcher: {error}"))
    .ok()?;
    watcher
        .watch(&parent, RecursiveMode::NonRecursive)
        .inspect_err(|error| log::warn!("failed to watch {}: {error}", parent.display()))
        .ok()?;
    Some(Watch { path, _watcher: watcher })
}

fn event_kind(kind: notify::EventKind) -> Option<&'static str> {
    match kind {
        notify::EventKind::Access(_) => None,
        notify::EventKind::Remove(_) => Some("unlink"),
        notify::EventKind::Create(_) => Some("add"),
        _ => Some("change"),
    }
}

async fn resolve(git: &Git, info: &LocationInfo) -> Option<Target> {
    match info.vcs.as_ref()? {
        Vcs::Git { .. } => {
            let repository = git.discover(&info.directory).await?;
            let resolved = resolved_path(&repository.git_directory);
            Some(Target {
                path: Path::new(&resolved).join("HEAD"),
                aliases: vec![".git".to_owned(), resolved, repository.git_directory],
            })
        }
        Vcs::Hg { store } => {
            let resolved = resolved_path(store);
            Some(Target { path: Path::new(&resolved).join("branch"), aliases: vec![".hg".to_owned(), resolved] })
        }
    }
}

fn resolved_path(path: &str) -> String {
    realpath(Path::new(path)).map_or_else(|| path.to_owned(), |resolved| resolved.to_string_lossy().into_owned())
}

fn location_ref(info: &LocationInfo) -> LocationRef {
    LocationRef {
        directory: AbsolutePath(info.directory.clone()),
        workspace_id: info.workspace_id.clone().map(WorkspaceID),
    }
}

fn publish(bus: &Bus, location: &LocationRef, event: &WatchEvent) {
    let data = json!({ "file": event.path.to_string_lossy(), "event": event.kind });
    let published = bus.publish(
        &Definition::ephemeral("filesystem.changed"),
        data,
        PublishOptions { location: Some(location.clone()), ..Default::default() },
    );
    if let Err(error) = published {
        log::warn!("failed to publish a filesystem change: {error}");
    }
}
