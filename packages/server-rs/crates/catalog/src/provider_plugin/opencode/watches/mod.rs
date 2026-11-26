pub mod transport;

use std::sync::{Arc, Mutex};

use futures_util::FutureExt;

use crate::provider_plugin::opencode::handle::ConsoleHandle;

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
}

impl Watches {
    /// Starts watching for `next`, or stops when it is `None`; a new workspace replaces the old one.
    pub(crate) fn sync(&self, handle: &Arc<ConsoleHandle>, next: Option<WatchTarget>) {
        let mut current = self.current.lock().expect("lock the Console watch lease");
        let previous = match (&*current, &next) {
            (None, None) => return,
            (Some(existing), Some(target)) if existing.key == target.key => {
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
        let weak = Arc::downgrade(handle);
        let deliver: transport::Deliver = Arc::new(move |events| {
            let weak = weak.clone();
            async move {
                match weak.upgrade() {
                    Some(handle) => handle.deliver_watched(events),
                    None => Vec::new(),
                }
            }
            .boxed()
        });
        let member = transport::join(target, deliver);
        *self.current.lock().expect("lock the Console watch lease") = Some(Current { key, member });
    }

    pub(crate) fn record(&self, events: Vec<WatchEvent>) -> Vec<String> {
        let ids = events.iter().map(|event| event.id.clone()).collect();
        if !events.is_empty() {
            self.delivered.lock().expect("lock the Console watched events").extend(events);
        }
        ids
    }

    pub(crate) fn delivered(&self) -> Vec<WatchEvent> {
        self.delivered.lock().expect("lock the Console watched events").clone()
    }
}

impl Drop for Watches {
    fn drop(&mut self) {
        let Some(current) = self.current.lock().expect("lock the Console watch lease").take() else {
            return;
        };
        transport::leave(&current.key, current.member);
    }
}
