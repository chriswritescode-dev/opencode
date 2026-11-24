use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::location::LocationRef;
use crate::map::{LocationMap, LocationServices};

pub const DEFAULT_TIME_TO_LIVE: Duration = Duration::from_secs(60 * 60);
pub const DEFAULT_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdleTimeout {
    After(Duration),
    Disabled,
}

pub trait ActivityProbe: Send + Sync + 'static {
    fn occupied(&self, reference: &LocationRef) -> bool;
}

pub type IdleTimeoutFn<T> = Box<dyn Fn(&T) -> Option<IdleTimeout> + Send + Sync>;

pub struct ActivityOptions<T: LocationServices> {
    pub time_to_live: Duration,
    pub sweep_interval: Duration,
    pub idle_timeout: IdleTimeoutFn<T>,
    pub probe: Option<Arc<dyn ActivityProbe>>,
}

impl<T: LocationServices> Default for ActivityOptions<T> {
    fn default() -> Self {
        ActivityOptions {
            time_to_live: DEFAULT_TIME_TO_LIVE,
            sweep_interval: DEFAULT_SWEEP_INTERVAL,
            idle_timeout: Box::new(|_| None),
            probe: None,
        }
    }
}

pub struct Activity<T: LocationServices> {
    state: Arc<State<T>>,
    task: JoinHandle<()>,
}

impl<T: LocationServices> Activity<T> {
    pub fn spawn(map: LocationMap<T>, options: ActivityOptions<T>) -> Self {
        let state = Arc::new(State { map, options, entries: Mutex::new(HashMap::new()) });
        let task = tokio::spawn(run(state.clone()));
        Activity { state, task }
    }

    pub async fn touch(&self, reference: &LocationRef) {
        if !self.state.map.has(reference).await {
            return;
        }
        self.state.touch(&reference.canonical());
    }
}

impl<T: LocationServices> Drop for Activity<T> {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct State<T: LocationServices> {
    map: LocationMap<T>,
    options: ActivityOptions<T>,
    entries: Mutex<HashMap<LocationRef, Instant>>,
}

impl<T: LocationServices> State<T> {
    fn touch(&self, reference: &LocationRef) {
        self.entries
            .lock()
            .expect("the activity entries lock is not poisoned")
            .insert(reference.clone(), Instant::now());
    }

    async fn sweep(&self) {
        let references = self.map.keys().await;
        let cached: HashSet<LocationRef> = references.iter().cloned().collect();
        let now = Instant::now();
        let tracked = {
            let mut entries = self.entries.lock().expect("the activity entries lock is not poisoned");
            for reference in &references {
                entries.entry(reference.clone()).or_insert(now);
            }
            entries.retain(|reference, _| cached.contains(reference));
            entries.iter().map(|(reference, touched_at)| (reference.clone(), *touched_at)).collect::<Vec<_>>()
        };

        let mut expired = Vec::new();
        for (reference, touched_at) in tracked {
            let services = self.map.services(&reference).await;
            let timeout = services.as_ref().and_then(|services| (self.options.idle_timeout)(services));
            let timeout = timeout.unwrap_or(IdleTimeout::After(self.options.time_to_live));
            let IdleTimeout::After(time_to_live) = timeout else { continue };
            if touched_at + time_to_live <= now {
                expired.push((reference, services));
            }
        }

        for (reference, services) in expired {
            let occupied = self.options.probe.as_ref().is_some_and(|probe| probe.occupied(&reference));
            let retained = services.as_ref().is_some_and(|services| services.lifecycle().is_retained());
            if occupied || retained {
                self.touch(&reference);
                continue;
            }
            self.entries.lock().expect("the activity entries lock is not poisoned").remove(&reference);
            log::info!(
                "location services evicted directory={} workspace_id={:?}",
                reference.directory,
                reference.workspace_id
            );
            self.map.invalidate(&reference).await;
        }
    }
}

async fn run<T: LocationServices>(state: Arc<State<T>>) {
    let mut shutdown = state.map.shutdown_signal();
    if *shutdown.borrow() {
        return;
    }
    loop {
        tokio::select! {
            _ = tokio::time::sleep(state.options.sweep_interval) => state.sweep().await,
            _ = shutdown.changed() => return,
        }
    }
}
