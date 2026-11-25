use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::broadcast;

use crate::host::{HostError, NpmPaths, NpmRequest, ScriptHost};

pub const INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub target: String,
    pub outdated: bool,
    pub updating: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct NpmPackage {
    pub directory: String,
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub revision: Option<String>,
}

#[derive(Clone, Copy)]
struct Status {
    outdated: bool,
    cycle: u64,
}

struct Inner {
    host: Option<Arc<dyn ScriptHost>>,
    paths: NpmPaths,
    locks: std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    status: std::sync::Mutex<HashMap<String, Status>>,
    changes: broadcast::Sender<Change>,
    intervals: broadcast::Sender<u64>,
    cycle: AtomicU64,
    started: AtomicBool,
}

#[derive(Clone)]
pub struct Updates {
    inner: Arc<Inner>,
}

impl Updates {
    pub fn new(host: Option<Arc<dyn ScriptHost>>, paths: NpmPaths) -> Updates {
        let (changes, _) = broadcast::channel(64);
        let (intervals, _) = broadcast::channel(64);
        Updates {
            inner: Arc::new(Inner {
                host,
                paths,
                locks: std::sync::Mutex::new(HashMap::new()),
                status: std::sync::Mutex::new(HashMap::new()),
                changes,
                intervals,
                cycle: AtomicU64::new(0),
                started: AtomicBool::new(false),
            }),
        }
    }

    pub fn cycle(&self) -> u64 {
        self.inner.cycle.load(Ordering::SeqCst)
    }

    pub fn changes(&self) -> broadcast::Receiver<Change> {
        self.inner.changes.subscribe()
    }

    pub fn intervals(&self) -> broadcast::Receiver<u64> {
        if !self.inner.started.swap(true, Ordering::SeqCst) {
            let updates = self.clone();
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval_at(tokio::time::Instant::now() + INTERVAL, INTERVAL);
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    ticker.tick().await;
                    let cycle = updates.inner.cycle.fetch_add(1, Ordering::SeqCst) + 1;
                    let _ = updates.inner.intervals.send(cycle);
                }
            });
        }
        self.inner.intervals.subscribe()
    }

    pub async fn check(&self, target: &str, refresh: bool) -> bool {
        let lock = self.lock(target);
        let _guard = lock.lock().await;
        let cycle = self.cycle();
        let previous = self.status(target);
        if let Some(status) = previous
            && !refresh
            && status.cycle == cycle
        {
            return status.outdated;
        }
        let outdated =
            match self.request(NpmRequest::Check { pkg: target.to_string(), paths: self.inner.paths.clone() }).await {
                Ok(value) => value.as_bool().unwrap_or(false),
                Err(error) => {
                    log::warn!("failed to check plugin update: target={target} error={error}");
                    previous.is_some_and(|status| status.outdated)
                }
            };
        self.inner
            .status
            .lock()
            .expect("the plugin update status lock is never poisoned")
            .insert(target.to_string(), Status { outdated, cycle });
        if previous.is_some_and(|status| status.outdated) != outdated {
            let _ = self.inner.changes.send(Change { target: target.to_string(), outdated, updating: false });
        }
        outdated
    }

    pub async fn update(&self, target: &str) -> Result<(), HostError> {
        let lock = self.lock(target);
        let _guard = lock.lock().await;
        let outdated = self.status(target).is_some_and(|status| status.outdated);
        let _ = self.inner.changes.send(Change { target: target.to_string(), outdated, updating: true });
        let result =
            self.request(NpmRequest::Update { pkg: target.to_string(), paths: self.inner.paths.clone() }).await;
        if result.is_ok() {
            self.inner.status.lock().expect("the plugin update status lock is never poisoned").remove(target);
        }
        let _ = self.inner.changes.send(Change {
            target: target.to_string(),
            outdated: if result.is_ok() { false } else { outdated },
            updating: false,
        });
        result.map(|_| ())
    }

    pub async fn follows(&self, target: &str) -> Result<bool, HostError> {
        let value =
            self.request(NpmRequest::Follows { pkg: target.to_string(), paths: self.inner.paths.clone() }).await?;
        Ok(value.as_bool().unwrap_or(false))
    }

    pub async fn resolve(&self, target: &str) -> Result<NpmPackage, HostError> {
        let value =
            self.request(NpmRequest::Resolve { pkg: target.to_string(), paths: self.inner.paths.clone() }).await?;
        serde_json::from_value(value)
            .map_err(|error| HostError { tag: "Protocol".to_string(), message: error.to_string() })
    }

    pub async fn reject(&self, target: &str, version: &str) -> Result<bool, HostError> {
        let value = self
            .request(NpmRequest::Reject {
                pkg: target.to_string(),
                version: version.to_string(),
                paths: self.inner.paths.clone(),
            })
            .await?;
        Ok(value.as_bool().unwrap_or(false))
    }

    fn status(&self, target: &str) -> Option<Status> {
        self.inner.status.lock().expect("the plugin update status lock is never poisoned").get(target).copied()
    }

    fn lock(&self, target: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.inner
            .locks
            .lock()
            .expect("the plugin update lock map is never poisoned")
            .entry(target.to_string())
            .or_default()
            .clone()
    }

    async fn request(&self, request: NpmRequest) -> Result<serde_json::Value, HostError> {
        let Some(host) = &self.inner.host else {
            return Err(HostError {
                tag: "Unavailable".to_string(),
                message: "the plugin host is not available".to_string(),
            });
        };
        host.npm(request).await
    }
}
