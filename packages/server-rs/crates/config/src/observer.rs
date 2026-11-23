use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::entry::Entry;
use crate::service::ConfigService;
use crate::watch::{WatchUpdate, contains};

const DEBOUNCE: Duration = Duration::from_millis(100);

pub struct ObserverHandle {
    entries: Arc<RwLock<Vec<Entry>>>,
    tasks: Vec<JoinHandle<()>>,
}

impl ObserverHandle {
    pub fn entries(&self) -> Vec<Entry> {
        self.entries.read().expect("the config observer lock is never poisoned").clone()
    }
}

impl Drop for ObserverHandle {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

pub fn observe(config: &ConfigService, reload: impl Fn(&[Entry]) + Send + 'static) -> ObserverHandle {
    let mut updated = config.subscribe_updated();
    let initial = config.entries();
    let entries = Arc::new(RwLock::new(initial));
    let task = {
        let config = config.clone();
        let entries = entries.clone();
        tokio::spawn(async move {
            loop {
                match updated.recv().await {
                    Ok(()) => {
                        let fresh = config.entries();
                        *entries.write().expect("the config observer lock is never poisoned") = fresh.clone();
                        reload(&fresh);
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        })
    };
    ObserverHandle { entries, tasks: vec![task] }
}

pub fn observe_sources<T: Send + 'static>(
    config: &ConfigService,
    directories: &'static [&'static str],
    load: impl Fn(&[Entry]) -> T + Send + 'static,
    apply: impl Fn(T) + Send + 'static,
) -> ObserverHandle {
    let mut changes = config.changes();
    let mut updated = config.subscribe_updated();
    let initial = config.entries();
    let entries = Arc::new(RwLock::new(initial.clone()));
    apply(load(&initial));

    let (notifications, mut receiver) = mpsc::unbounded_channel();

    let changes_task = {
        let config = config.clone();
        let notifications = notifications.clone();
        tokio::spawn(async move {
            loop {
                match changes.recv().await {
                    Ok(WatchUpdate { path }) => {
                        let entries = config.entries();
                        if is_directory_source(&entries, &path, directories) {
                            let _ = notifications.send(());
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        })
    };

    let updated_task = {
        let notifications = notifications.clone();
        tokio::spawn(async move {
            loop {
                match updated.recv().await {
                    Ok(()) => {
                        let _ = notifications.send(());
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        })
    };

    drop(notifications);

    let debounce_task = {
        let config = config.clone();
        let entries = entries.clone();
        tokio::spawn(async move {
            loop {
                if receiver.recv().await.is_none() {
                    return;
                }
                loop {
                    match tokio::time::timeout(DEBOUNCE, receiver.recv()).await {
                        Ok(Some(())) => continue,
                        Ok(None) => return,
                        Err(_) => break,
                    }
                }
                let fresh = config.entries();
                *entries.write().expect("the config observer lock is never poisoned") = fresh.clone();
                apply(load(&fresh));
            }
        })
    };

    ObserverHandle { entries, tasks: vec![changes_task, updated_task, debounce_task] }
}

pub fn is_directory_source(entries: &[Entry], file: &Path, names: &[&str]) -> bool {
    entries.iter().any(|entry| match entry {
        Entry::Directory { path } => names.iter().any(|name| contains(&path.join(name), file)),
        Entry::Document(_) => false,
    })
}
