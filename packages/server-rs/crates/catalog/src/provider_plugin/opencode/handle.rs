use std::sync::RwLock;

use opencode_contract::generated::{ConfigEntryDocumentInfo, ConnectionStatus};
use tokio::sync::{Mutex, watch};

use super::Snapshot;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Allowed {
    pub watches: bool,
    pub lists: bool,
    pub artifacts: bool,
}

pub struct ConsoleHandle {
    pub(crate) snapshot: RwLock<Snapshot>,
    pub(crate) integrated: RwLock<Option<ConfigEntryDocumentInfo>>,
    pub(crate) allowed: RwLock<Allowed>,
    pub(crate) reported: RwLock<Option<ConnectionStatus>>,
    pub(crate) loading: Mutex<()>,
    revision: watch::Sender<u64>,
}

impl Default for ConsoleHandle {
    fn default() -> Self {
        Self {
            snapshot: RwLock::new(Snapshot::default()),
            integrated: RwLock::new(None),
            allowed: RwLock::new(Allowed::default()),
            reported: RwLock::new(None),
            loading: Mutex::new(()),
            revision: watch::channel(0).0,
        }
    }
}

impl ConsoleHandle {
    pub fn changes(&self) -> watch::Receiver<u64> {
        self.revision.subscribe()
    }

    pub fn allowed(&self) -> Allowed {
        *self.allowed.read().expect("lock the Console allowed flags")
    }

    pub(crate) fn snapshot(&self) -> Snapshot {
        self.snapshot.read().expect("lock the Console snapshot").clone()
    }

    pub(crate) fn set_snapshot(&self, value: Snapshot) {
        *self.snapshot.write().expect("lock the Console snapshot") = value;
    }

    pub(crate) fn integrated(&self) -> Option<ConfigEntryDocumentInfo> {
        self.integrated.read().expect("lock the Console integrated document").clone()
    }

    pub(crate) fn set_integrated(&self, value: Option<ConfigEntryDocumentInfo>) {
        *self.integrated.write().expect("lock the Console integrated document") = value;
    }

    pub(crate) fn set_allowed(&self, value: Allowed) {
        *self.allowed.write().expect("lock the Console allowed flags") = value;
    }

    pub(crate) fn set_reported(&self, value: Option<ConnectionStatus>) {
        *self.reported.write().expect("lock the Console reported status") = value;
    }

    pub(crate) fn bump(&self) {
        self.revision.send_modify(|revision| *revision += 1);
    }
}
