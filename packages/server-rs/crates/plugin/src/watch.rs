use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use notify::event::{AccessKind, AccessMode, EventKind};
use notify::{RecursiveMode, Watcher as _};
use tokio::sync::broadcast;

pub trait PluginWatcher: Send + Sync {
    fn watch(&self, path: &Path) -> io::Result<()>;
    fn changes(&self) -> broadcast::Receiver<()>;
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub enum WatchTarget {
    File(PathBuf),
    Tree(PathBuf),
    Parent(PathBuf),
}

impl WatchTarget {
    fn requested(&self) -> &Path {
        match self {
            WatchTarget::File(file) => file,
            WatchTarget::Tree(path) | WatchTarget::Parent(path) => path,
        }
    }

    fn root(&self) -> &Path {
        match self {
            WatchTarget::File(file) => file.parent().unwrap_or(file),
            WatchTarget::Tree(path) | WatchTarget::Parent(path) => path,
        }
    }

    fn recursive(&self) -> bool {
        matches!(self, WatchTarget::Tree(_))
    }

    fn matches(&self, path: &Path) -> bool {
        match self {
            WatchTarget::File(file) => path == file,
            WatchTarget::Tree(root) => path.starts_with(root),
            WatchTarget::Parent(root) => path == root || path.parent() == Some(root.as_path()),
        }
    }
}

pub fn watch_target(path: &Path) -> io::Result<WatchTarget> {
    let mut current = path;
    loop {
        if let Ok(metadata) = std::fs::metadata(current) {
            return Ok(match (current == path, metadata.is_dir()) {
                (true, true) => WatchTarget::Tree(current.to_path_buf()),
                (true, false) => WatchTarget::File(current.to_path_buf()),
                (false, _) => WatchTarget::Parent(current.to_path_buf()),
            });
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("no existing ancestor for {}", path.display()),
                ));
            }
        }
    }
}

fn reloads(event: &notify::Event) -> bool {
    matches!(
        event.kind,
        EventKind::Create(_)
            | EventKind::Remove(_)
            | EventKind::Modify(_)
            | EventKind::Access(AccessKind::Close(AccessMode::Write))
    )
}

pub struct NotifyWatcher {
    watcher: Mutex<Option<notify::RecommendedWatcher>>,
    changes: broadcast::Sender<()>,
    watched: Arc<Mutex<HashSet<WatchTarget>>>,
    disabled: bool,
}

impl NotifyWatcher {
    pub fn new() -> NotifyWatcher {
        let (changes, _) = broadcast::channel(16);
        let watched: Arc<Mutex<HashSet<WatchTarget>>> = Arc::new(Mutex::new(HashSet::new()));
        let watcher = if opencode_local::watch::disabled(|name| std::env::var(name).ok()) {
            None
        } else {
            let sender = changes.clone();
            let targets = Arc::clone(&watched);
            let created = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                let Ok(event) = event else { return };
                if !reloads(&event) {
                    return;
                }
                let targets = targets.lock().expect("the plugin watcher set is never poisoned");
                if event.paths.iter().any(|path| targets.iter().any(|target| target.matches(path))) {
                    let _ = sender.send(());
                }
            });
            match created {
                Ok(watcher) => Some(watcher),
                Err(error) => {
                    log::warn!("failed to create the plugin watcher, live reload disabled: {error}");
                    None
                }
            }
        };
        let disabled = watcher.is_none();
        NotifyWatcher { watcher: Mutex::new(watcher), changes, watched, disabled }
    }
}

impl Default for NotifyWatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginWatcher for NotifyWatcher {
    fn watch(&self, path: &Path) -> io::Result<()> {
        let target = watch_target(path)?;
        {
            let mut watched = self.watched.lock().expect("the plugin watcher set is never poisoned");
            if !watched.insert(target.clone()) {
                return Ok(());
            }
        }
        if self.disabled {
            return Ok(());
        }
        let mut watcher = self.watcher.lock().expect("the plugin watcher is never poisoned");
        let Some(watcher) = watcher.as_mut() else { return Ok(()) };
        let mode = if target.recursive() { RecursiveMode::Recursive } else { RecursiveMode::NonRecursive };
        if let Err(error) = watcher.watch(target.root(), mode) {
            log::warn!("failed to watch {}, live reload disabled: {error}", target.root().display());
        }
        Ok(())
    }

    fn changes(&self) -> broadcast::Receiver<()> {
        self.changes.subscribe()
    }
}

pub struct ManualWatcher {
    paths: Mutex<Vec<PathBuf>>,
    changes: broadcast::Sender<()>,
}

impl ManualWatcher {
    pub fn new() -> ManualWatcher {
        let (changes, _) = broadcast::channel(16);
        ManualWatcher { paths: Mutex::new(Vec::new()), changes }
    }

    pub fn watched(&self) -> Vec<PathBuf> {
        self.paths.lock().expect("the manual watcher set is never poisoned").clone()
    }

    pub fn emit(&self) {
        let _ = self.changes.send(());
    }
}

impl Default for ManualWatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginWatcher for ManualWatcher {
    fn watch(&self, path: &Path) -> io::Result<()> {
        let target = watch_target(path)?.requested().to_path_buf();
        let mut paths = self.paths.lock().expect("the manual watcher set is never poisoned");
        if !paths.contains(&target) {
            paths.push(target);
        }
        Ok(())
    }

    fn changes(&self) -> broadcast::Receiver<()> {
        self.changes.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use notify::event::{AccessKind, AccessMode, CreateKind, DataChange, ModifyKind, RemoveKind, RenameMode};

    use super::*;

    fn event(kind: EventKind) -> notify::Event {
        notify::Event::new(kind)
    }

    #[test]
    fn reloads_on_changes_and_write_closes_only() {
        assert!(reloads(&event(EventKind::Create(CreateKind::File))));
        assert!(reloads(&event(EventKind::Create(CreateKind::Folder))));
        assert!(reloads(&event(EventKind::Modify(ModifyKind::Data(DataChange::Any)))));
        assert!(reloads(&event(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))));
        assert!(reloads(&event(EventKind::Remove(RemoveKind::File))));
        assert!(reloads(&event(EventKind::Access(AccessKind::Close(AccessMode::Write)))));
        assert!(!reloads(&event(EventKind::Access(AccessKind::Close(AccessMode::Read)))));
        assert!(!reloads(&event(EventKind::Access(AccessKind::Close(AccessMode::Any)))));
        assert!(!reloads(&event(EventKind::Access(AccessKind::Open(AccessMode::Any)))));
        assert!(!reloads(&event(EventKind::Access(AccessKind::Read))));
        assert!(!reloads(&event(EventKind::Any)));
        assert!(!reloads(&event(EventKind::Other)));
    }

    fn workspace() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "opencode-plugin-watch-{}-{}",
            std::process::id(),
            rand::random::<u32>()
        ));
        std::fs::create_dir_all(root.join("plugin")).expect("create the target directory");
        std::fs::write(root.join("plugin.ts"), "export default {}").expect("write the target file");
        std::fs::write(root.join("sibling.ts"), "export default {}").expect("write the sibling file");
        root
    }

    #[test]
    fn watches_a_directory_recursively() {
        let root = workspace();
        let directory = root.join("plugin");
        let target = watch_target(&directory).expect("the directory resolves");
        assert_eq!(target.root(), directory);
        assert!(target.recursive());
        assert!(target.matches(&directory.join("nested").join("helper.ts")));
        std::fs::remove_dir_all(&root).expect("remove the temp directory");
    }

    #[test]
    fn watches_a_file_through_its_parent_directory() {
        let root = workspace();
        let file = root.join("plugin.ts");
        let target = watch_target(&file).expect("the file resolves");
        assert_eq!(target.root(), root);
        assert_eq!(target.requested(), file);
        assert!(!target.recursive());
        assert!(target.matches(&file), "a replace of the file itself reloads");
        assert!(!target.matches(&root.join("sibling.ts")), "a sibling in the same directory does not");
        std::fs::remove_dir_all(&root).expect("remove the temp directory");
    }

    #[test]
    fn watches_a_missing_targets_nearest_ancestor_without_recursion() {
        let root = workspace();
        let missing = root.join("missing");
        let target = watch_target(&missing.join("nested")).expect("the ancestor resolves");
        assert_eq!(target.root(), root);
        assert!(!target.recursive());
        assert!(target.matches(&missing));
        assert!(target.matches(&root));
        assert!(!target.matches(&missing.join("nested")));
        std::fs::remove_dir_all(&root).expect("remove the temp directory");
    }

    #[tokio::test]
    async fn keeps_watching_a_file_that_is_replaced_atomically() {
        let root = workspace();
        let file = root.join("plugin.ts");
        let watcher = NotifyWatcher::new();
        let mut changes = watcher.changes();
        watcher.watch(&file).expect("the file is watched");
        tokio::time::sleep(Duration::from_millis(100)).await;

        let temp = root.join("plugin.ts.tmp");
        std::fs::write(&temp, "export default { atomic: true }").expect("write the temp file");
        std::fs::rename(&temp, &file).expect("replace the file");
        tokio::time::timeout(Duration::from_secs(5), changes.recv())
            .await
            .expect("the replace reloads")
            .expect("the watcher is open");
        while tokio::time::timeout(Duration::from_millis(200), changes.recv()).await.is_ok() {}

        std::fs::write(&file, "export default { atomic: false }").expect("write the file again");
        tokio::time::timeout(Duration::from_secs(5), changes.recv())
            .await
            .expect("a later edit reloads after the replace")
            .expect("the watcher is open");

        std::fs::remove_dir_all(&root).expect("remove the temp directory");
    }
}
