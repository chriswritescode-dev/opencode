use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use notify::event::{AccessKind, AccessMode, EventKind};
use notify::{RecursiveMode, Watcher as _};
use tokio::sync::broadcast;

pub trait PluginWatcher: Send + Sync {
    fn watch(&self, path: &Path) -> io::Result<()>;
    fn changes(&self) -> broadcast::Receiver<()>;
}

pub fn watch_target(path: &Path) -> io::Result<(PathBuf, bool)> {
    let mut current = path;
    loop {
        if let Ok(metadata) = std::fs::metadata(current) {
            return Ok((current.to_path_buf(), current == path && metadata.is_dir()));
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
    watched: Mutex<HashSet<PathBuf>>,
    disabled: bool,
}

impl NotifyWatcher {
    pub fn new() -> NotifyWatcher {
        let (changes, _) = broadcast::channel(16);
        let watcher = if opencode_local::watch::disabled(|name| std::env::var(name).ok()) {
            None
        } else {
            let sender = changes.clone();
            let created = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                if event.is_ok_and(|event| reloads(&event)) {
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
        NotifyWatcher { watcher: Mutex::new(watcher), changes, watched: Mutex::new(HashSet::new()), disabled }
    }
}

impl Default for NotifyWatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginWatcher for NotifyWatcher {
    fn watch(&self, path: &Path) -> io::Result<()> {
        let (target, recursive) = watch_target(path)?;
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
        let mode = if recursive { RecursiveMode::Recursive } else { RecursiveMode::NonRecursive };
        if let Err(error) = watcher.watch(&target, mode) {
            log::warn!("failed to watch {}, live reload disabled: {error}", target.display());
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
        let (target, _) = watch_target(path)?;
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

    #[test]
    fn watches_only_an_existing_directory_recursively() {
        let root = std::env::temp_dir().join(format!(
            "opencode-plugin-watch-{}-{}",
            std::process::id(),
            rand::random::<u32>()
        ));
        let directory = root.join("plugin");
        let file = root.join("plugin.ts");
        std::fs::create_dir_all(&directory).expect("create the target directory");
        std::fs::write(&file, "export default {}").expect("write the target file");

        assert_eq!(watch_target(&directory).expect("the directory resolves"), (directory.clone(), true));
        assert_eq!(watch_target(&file).expect("the file resolves"), (file.clone(), false));
        assert_eq!(
            watch_target(&root.join("missing").join("nested")).expect("the ancestor resolves"),
            (root.clone(), false)
        );

        std::fs::remove_dir_all(&root).expect("remove the temp directory");
    }
}
