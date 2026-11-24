use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use opencode_bus::feed::{EventFeed, SUBSCRIBER_CAPACITY};
use opencode_bus::{Bus, BusOptions};
use opencode_session::{ProjectionSettings, SessionProjector, SessionStore};
use opencode_storage::{Database, DatabaseLocation, MigrationReport, StorageError};
use tokio::task::JoinHandle;

pub struct ServerState {
    pub db: Arc<Database>,
    pub bus: Arc<Bus>,
    pub feed: Arc<EventFeed>,
    pub store: SessionStore,
    pub usage: JoinHandle<()>,
    pub heartbeat: Duration,
}

impl Drop for ServerState {
    fn drop(&mut self) {
        self.usage.abort();
    }
}

impl ServerState {
    pub fn open(location: DatabaseLocation, data: &Path) -> Result<(ServerState, MigrationReport), StorageError> {
        let (db, report) = Database::open(location, data)?;
        let db = Arc::new(db);
        let bus = Bus::new(db.clone(), BusOptions { persist: false, ..Default::default() });
        let usage = SessionProjector::start(&bus, &db, Arc::new(ProjectionSettings::default));
        let feed = EventFeed::new(&bus, SUBSCRIBER_CAPACITY);
        let store = SessionStore::new(db.clone());
        let state = ServerState { db, bus, feed, store, usage, heartbeat: Duration::from_secs(15) };
        Ok((state, report))
    }
}
