use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use notify::{RecursiveMode, Watcher as NotifyWatcher};

pub const DEBOUNCE: Duration = Duration::from_millis(100);

pub type VarReader = std::sync::Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

pub struct WatchHandle {
    _watcher: Option<notify::RecommendedWatcher>,
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
    let mut directories = BTreeSet::new();
    let mut interesting = BTreeSet::new();
    for path in paths {
        plan(&path, &mut directories, &mut interesting);
    }
    if directories.is_empty() {
        return dormant();
    }
    let (sender, receiver) = mpsc::channel::<()>();
    let watched = interesting;
    let Ok(mut watcher) = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else { return };
        if event.paths.iter().any(|path| matches(path, &watched)) {
            let _ = sender.send(());
        }
    }) else {
        return dormant();
    };
    for directory in &directories {
        if let Err(error) = watcher.watch(directory, RecursiveMode::NonRecursive) {
            log::warn!("failed to watch {}: {error}", directory.display());
        }
    }
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

fn plan(path: &Path, directories: &mut BTreeSet<PathBuf>, interesting: &mut BTreeSet<PathBuf>) {
    if let Ok(canonical) = path.canonicalize() {
        interesting.insert(canonical.clone());
        if let Some(parent) = canonical.parent() {
            directories.insert(parent.to_path_buf());
        }
        if let Some(parent) = path.parent() {
            let parent = parent.canonicalize().unwrap_or_else(|_| parent.to_path_buf());
            if let Some(name) = path.file_name() {
                interesting.insert(parent.join(name));
            }
            directories.insert(parent);
        }
        return;
    }
    let Some(ancestor) = path.ancestors().skip(1).find(|ancestor| ancestor.is_dir()) else { return };
    let Some(first) = path.strip_prefix(ancestor).ok().and_then(|relative| relative.components().next()) else {
        return;
    };
    let ancestor = ancestor.canonicalize().unwrap_or_else(|_| ancestor.to_path_buf());
    interesting.insert(ancestor.join(first.as_os_str()));
    directories.insert(ancestor);
}

fn matches(path: &Path, interesting: &BTreeSet<PathBuf>) -> bool {
    if interesting.contains(path) {
        return true;
    }
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else { return false };
    let parent = parent.canonicalize().unwrap_or_else(|_| parent.to_path_buf());
    interesting.contains(&parent.join(name))
}
