use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use opencode_bus::feed::{EventFeed, SUBSCRIBER_CAPACITY};
use opencode_bus::{Bus, BusOptions, ListenerHandle};
use opencode_session::{ProjectionSettings, SessionProjector, SessionStore};
use opencode_storage::{Database, DatabaseLocation, MigrationReport, StorageError};

pub struct ServerState {
    pub db: Arc<Database>,
    pub bus: Arc<Bus>,
    pub feed: Arc<EventFeed>,
    pub store: SessionStore,
    pub projector: ListenerHandle,
    pub heartbeat: Duration,
}

impl ServerState {
    pub fn open(location: DatabaseLocation, data: &Path) -> Result<(ServerState, MigrationReport), StorageError> {
        let (db, report) = Database::open(location, data)?;
        let db = Arc::new(db);
        let bus = Bus::new(db.clone(), BusOptions { persist: false, ..Default::default() });
        let projector = SessionProjector::register(&bus, &db, Arc::new(ProjectionSettings::default));
        let feed = EventFeed::new(&bus, SUBSCRIBER_CAPACITY);
        let store = SessionStore::new(db.clone());
        let state = ServerState { db, bus, feed, store, projector, heartbeat: Duration::from_secs(15) };
        Ok((state, report))
    }
}
