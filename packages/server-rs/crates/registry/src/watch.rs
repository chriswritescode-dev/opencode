use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use opencode_fs::watcher::{Subscription, WatchInput, Watcher};
use tokio::sync::broadcast::error::RecvError;

pub const DEBOUNCE: Duration = Duration::from_millis(100);

pub type VarReader = std::sync::Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

pub struct WatchHandle {
    _watcher: Option<Watcher>,
    _debounce: Option<thread::JoinHandle<()>>,
}

pub fn watch_files(
    paths: impl IntoIterator<Item = PathBuf>,
    var: impl Fn(&str) -> Option<String>,
    mut on_change: impl FnMut() + Send + 'static,
) -> WatchHandle {
    if disabled(&var) {
        return dormant();
    }
    let mut targets = BTreeMap::new();
    for path in paths {
        plan(&path, &mut targets);
    }
    if targets.is_empty() {
        return dormant();
    }
    let watcher = Watcher::new(true);
    let subscriptions: Vec<Subscription> = targets
        .into_iter()
        .map(|(directory, names)| {
            watcher.subscribe(WatchInput::Entries { path: directory, names: names.into_iter().collect() })
        })
        .collect();
    let (sender, receiver) = mpsc::channel::<()>();
    for mut subscription in subscriptions {
        let sender = sender.clone();
        thread::spawn(move || {
            loop {
                match subscription.blocking_recv() {
                    Ok(_) | Err(RecvError::Lagged(_)) => {
                        if sender.send(()).is_err() {
                            return;
                        }
                    }
                    Err(RecvError::Closed) => return,
                }
            }
        });
    }
    drop(sender);
    let debounce = thread::spawn(move || {
        while receiver.recv().is_ok() {
            loop {
                match receiver.recv_timeout(DEBOUNCE) {
                    Ok(()) => continue,
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
            on_change();
        }
    });
    WatchHandle { _watcher: Some(watcher), _debounce: Some(debounce) }
}

fn dormant() -> WatchHandle {
    WatchHandle { _watcher: None, _debounce: None }
}

fn disabled(var: impl Fn(&str) -> Option<String>) -> bool {
    var("OPENCODE_FILEWATCHER_DISABLE")
        .or_else(|| var("OPENCODE_DISABLE_FILEWATCHER"))
        .is_some_and(|value| value == "1" || value.to_lowercase() == "true")
}

fn plan(path: &Path, targets: &mut BTreeMap<PathBuf, BTreeSet<String>>) {
    if let Ok(canonical) = path.canonicalize() {
        if let (Some(parent), Some(name)) = (canonical.parent(), canonical.file_name()) {
            targets.entry(parent.to_path_buf()).or_default().insert(name.to_string_lossy().into_owned());
        }
        if let Some(parent) = path.parent() {
            let parent = parent.canonicalize().unwrap_or_else(|_| parent.to_path_buf());
            if let Some(name) = path.file_name() {
                targets.entry(parent).or_default().insert(name.to_string_lossy().into_owned());
            }
        }
        return;
    }
    let Some(ancestor) = path.ancestors().skip(1).find(|ancestor| ancestor.is_dir()) else { return };
    let Some(first) = path.strip_prefix(ancestor).ok().and_then(|relative| relative.components().next()) else {
        return;
    };
    let ancestor = ancestor.canonicalize().unwrap_or_else(|_| ancestor.to_path_buf());
    targets.entry(ancestor).or_default().insert(first.as_os_str().to_string_lossy().into_owned());
}
