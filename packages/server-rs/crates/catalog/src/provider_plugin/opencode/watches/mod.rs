pub mod tools;
pub mod transport;
pub mod wake;

use std::sync::{Arc, Mutex};

use futures_util::FutureExt;

use crate::provider_plugin::opencode::handle::ConsoleHandle;

pub use self::tools::{ShellOutput, ToolFailure, WatchTools, shell_target};
pub use self::transport::{ACK_LIMIT, LEASE_CLOSE_CODE, OVERSIZED_CLOSE_CODE, WatchActor, WatchEvent};

pub(crate) use self::transport::WatchTarget;

/// The `GET /api/v2/config` response header Console sends while the workspace has event watches.
pub const HEADER: &str = "x-opencode-event-watches";

#[derive(Default)]
pub(crate) struct Watches {
    current: Mutex<Option<Current>>,
    delivered: Mutex<Vec<WatchEvent>>,
}

struct Current {
    key: String,
    member: u64,
    console: Arc<Mutex<wake::Console>>,
    dependencies: wake::Dependencies,
}

impl Watches {
    /// Starts watching for `next`, or stops when it is `None`; a new workspace replaces the old one.
    pub(crate) fn sync(&self, handle: &Arc<ConsoleHandle>, next: Option<WatchTarget>) {
        let mut current = self.current.lock().expect("lock the Console watch lease");
        let previous = match (&*current, &next) {
            (None, None) => return,
            (Some(existing), Some(target)) if existing.key == target.key => {
                *existing.console.lock().expect("lock the Console watch connection") = console(target);
                let key = existing.key.clone();
                let member = existing.member;
                let target = next.expect("the Console watch target being synced");
                drop(current);
                transport::update(&key, member, target);
                return;
            }
            _ => current.take(),
        };
        drop(current);
        if let Some(previous) = previous {
            transport::leave(&previous.key, previous.member);
        }
        let Some(target) = next else {
            return;
        };
        let key = target.key.clone();
        let connection = Arc::new(Mutex::new(console(&target)));
        let dependencies = wake::Dependencies {
            sessions: target.sessions.clone(),
            kv: target.kv.clone(),
            clock: target.clock.clone(),
        };
        let weak = Arc::downgrade(handle);
        let deliver_connection = connection.clone();
        let deliver_dependencies = dependencies.clone();
        let deliver: transport::Deliver = Arc::new(move |events| {
            let weak = weak.clone();
            let connection = deliver_connection.clone();
            let dependencies = deliver_dependencies.clone();
            async move {
                let Some(handle) = weak.upgrade() else {
                    return Vec::new();
                };
                handle.watches.record(events.clone());
                let connection = connection.lock().expect("lock the Console watch connection").clone();
                wake::deliver(&dependencies, &connection, events).await
            }
            .boxed()
        });
        let member = transport::join(target, deliver);
        *self.current.lock().expect("lock the Console watch lease") =
            Some(Current { key, member, console: connection, dependencies });
    }

    pub(crate) fn is_leader(&self) -> bool {
        let current = self.current.lock().expect("lock the Console watch lease");
        current.as_ref().is_some_and(|current| transport::is_leader(&current.key, current.member))
    }

    pub(crate) fn tools(&self) -> Option<WatchTools> {
        let current = self.current.lock().expect("lock the Console watch lease");
        let current = current.as_ref()?;
        Some(WatchTools::new(
            current.console.clone(),
            current.dependencies.kv.clone(),
            current.dependencies.sessions.clone(),
        ))
    }

    pub(crate) async fn forget(&self, session_id: &str) {
        let (connection, dependencies) = {
            let current = self.current.lock().expect("lock the Console watch lease");
            match current.as_ref() {
                Some(current) => (
                    current.console.lock().expect("lock the Console watch connection").clone(),
                    current.dependencies.clone(),
                ),
                None => return,
            }
        };
        wake::forget(&dependencies, &connection, session_id).await;
    }

    fn record(&self, events: Vec<WatchEvent>) {
        if !events.is_empty() {
            self.delivered.lock().expect("lock the Console watched events").extend(events);
        }
    }

    pub(crate) fn delivered(&self) -> Vec<WatchEvent> {
        self.delivered.lock().expect("lock the Console watched events").clone()
    }
}

fn console(target: &WatchTarget) -> wake::Console {
    wake::Console { server: target.server.clone(), headers: target.headers.clone(), http: target.http.clone() }
}

impl Drop for Watches {
    fn drop(&mut self) {
        let Some(current) = self.current.lock().expect("lock the Console watch lease").take() else {
            return;
        };
        transport::leave(&current.key, current.member);
    }
}
