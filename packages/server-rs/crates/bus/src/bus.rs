use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use opencode_contract::codec::encode_json;
use opencode_contract::generated::LocationRef;
use opencode_storage::Database;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Map, Value};
use tokio::sync::Notify;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::decode;
use crate::error::BusError;
use crate::event::{Definition, DurableSpec, Event, EventId, versioned_type};
use crate::legacy;
use crate::lock::KeyedMutex;
use crate::route::Routes;

type Delivery = (Arc<Event>, Option<Arc<[LocationRef]>>);
type Projector = Arc<dyn Fn(&Connection, &Event) -> Result<(), BusError> + Send + Sync>;
type ProjectorRegistration = Box<dyn Fn(&Connection, &Event) -> Result<(), BusError> + Send + Sync>;
type Listener = Arc<dyn Fn(&Event) -> Result<(), BusError> + Send + Sync>;
type ListenerRegistration = Box<dyn Fn(&Event) -> Result<(), BusError> + Send + Sync>;
type CommitHook = Box<dyn FnOnce(&Connection, i64) -> Result<(), BusError>>;
type BeforeAggregateRead = Arc<dyn Fn(&str) + Send + Sync>;
type Now = Arc<dyn Fn() -> i64 + Send + Sync>;

pub struct BusOptions {
    pub persist: bool,
    pub log_read_page_size: usize,
    pub before_aggregate_read: Option<BeforeAggregateRead>,
    pub now: Option<Now>,
}

impl Default for BusOptions {
    fn default() -> Self {
        BusOptions { persist: false, log_read_page_size: 512, before_aggregate_read: None, now: None }
    }
}

#[derive(Default)]
pub struct PublishOptions {
    pub id: Option<String>,
    pub metadata: Option<Map<String, Value>>,
    pub location: Option<LocationRef>,
    pub global: bool,
    pub commit: Option<CommitHook>,
}

pub struct PublishInput {
    pub definition: Definition,
    pub data: Value,
    pub options: PublishOptions,
}

pub struct SerializedEvent {
    pub id: String,
    pub r#type: String,
    pub created: Option<i64>,
    pub seq: i64,
    pub aggregate_id: String,
    pub data: Value,
}

#[derive(Default)]
pub struct ReplayOptions {
    pub publish: bool,
    pub owner_id: Option<String>,
    pub strict_owner: bool,
}

pub struct Bus {
    pub(crate) db: Arc<Database>,
    pub(crate) options: BusOptions,
    projectors: Mutex<HashMap<String, Vec<Projector>>>,
    listeners: Arc<Mutex<Vec<(u64, Listener)>>>,
    next_listener_id: AtomicU64,
    pub(crate) durable: Arc<Mutex<HashMap<String, Vec<Arc<Notify>>>>>,
    subscribers: Arc<Mutex<Vec<Subscriber>>>,
    next_subscriber_id: AtomicU64,
    routes: Mutex<Routes>,
    locks: KeyedMutex<String>,
}

struct Subscriber {
    id: u64,
    types: Option<Vec<String>>,
    sender: UnboundedSender<Delivery>,
}

struct Prepared {
    event: Event,
    r#type: &'static str,
    spec: DurableSpec,
    aggregate_id: String,
    commit: Option<CommitHook>,
}

struct EventRow {
    id: String,
    seq: i64,
    created: i64,
    r#type: String,
    data: String,
}

pub struct Subscription {
    receiver: UnboundedReceiver<Delivery>,
    subscribers: Arc<Mutex<Vec<Subscriber>>>,
    id: u64,
    location: Option<LocationRef>,
}

impl Subscription {
    pub fn try_recv(&mut self) -> Option<Arc<Event>> {
        loop {
            let (event, routes) = self.receiver.try_recv().ok()?;
            if accepts(self.location.as_ref(), &event, routes.as_deref()) {
                return Some(event);
            }
        }
    }

    pub async fn recv(&mut self) -> Option<Arc<Event>> {
        loop {
            let (event, routes) = self.receiver.recv().await?;
            if accepts(self.location.as_ref(), &event, routes.as_deref()) {
                return Some(event);
            }
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        lock(&self.subscribers).retain(|subscriber| subscriber.id != self.id);
    }
}

pub struct ListenerHandle {
    listeners: Arc<Mutex<Vec<(u64, Listener)>>>,
    id: u64,
}

impl Drop for ListenerHandle {
    fn drop(&mut self) {
        lock(&self.listeners).retain(|(id, _)| *id != self.id);
    }
}

impl Bus {
    pub fn new(db: Arc<Database>, options: BusOptions) -> Arc<Bus> {
        Arc::new(Bus {
            db,
            options,
            projectors: Mutex::new(HashMap::new()),
            listeners: Arc::new(Mutex::new(Vec::new())),
            next_listener_id: AtomicU64::new(0),
            durable: Arc::new(Mutex::new(HashMap::new())),
            subscribers: Arc::new(Mutex::new(Vec::new())),
            next_subscriber_id: AtomicU64::new(0),
            routes: Mutex::new(Routes::default()),
            locks: KeyedMutex::default(),
        })
    }

    pub fn publish(&self, definition: &Definition, data: Value, options: PublishOptions) -> Result<Event, BusError> {
        let Some(spec) = definition.durable else {
            if options.commit.is_some() {
                return Err(BusError::InvalidDurableEvent {
                    r#type: definition.r#type.to_string(),
                    message: "Local commit hooks require a durable event".to_string(),
                });
            }
            let event = self.make_payload(definition, data, &options);
            let mut routes = lock(&self.routes);
            let pending = self.db.read(|connection| Ok(routes.prepare(connection, &[&event])?))?;
            pending.apply(&mut routes);
            let resolved = pending.routes(0);
            drop(routes);
            self.notify(&event, resolved, false)?;
            return Ok(event);
        };
        let aggregate_id = aggregate_id(definition.r#type, spec.aggregate, &data)?;
        let event = self.make_payload(definition, data, &options);
        let commit = options.commit;
        self.locks.with(aggregate_id.clone(), || self.commit_one(definition.r#type, spec, &aggregate_id, event, commit))
    }

    pub fn publish_all(&self, events: Vec<PublishInput>) -> Result<Vec<Event>, BusError> {
        if events.is_empty() {
            return Ok(Vec::new());
        }
        let mut prepared = Vec::with_capacity(events.len());
        for input in events {
            let r#type = input.definition.r#type;
            let Some(spec) = input.definition.durable else {
                return Err(BusError::InvalidDurableEvent {
                    r#type: r#type.to_string(),
                    message: "Published events must be durable".to_string(),
                });
            };
            let aggregate_id = aggregate_id(r#type, spec.aggregate, &input.data)?;
            let event = self.make_payload(&input.definition, input.data, &input.options);
            prepared.push(Prepared { event, r#type, spec, aggregate_id, commit: input.options.commit });
        }
        let aggregate_id = prepared[0].aggregate_id.clone();
        if prepared.iter().any(|item| item.aggregate_id != aggregate_id) {
            return Err(BusError::InvalidDurableEvent {
                r#type: prepared[0].r#type.to_string(),
                message: "Published events must belong to the same aggregate".to_string(),
            });
        }
        self.locks.with(aggregate_id.clone(), || self.commit_batch(&aggregate_id, prepared))
    }

    pub fn replay(&self, event: SerializedEvent, options: ReplayOptions) -> Result<(), BusError> {
        let Some(definition) = Definition::durable_lookup(&event.r#type) else {
            return Err(BusError::unknown_durable_event(&event.r#type));
        };
        let spec = definition.durable.expect("a resolved durable definition carries its spec");
        let versioned = versioned_type(definition.r#type, spec.version);
        self.locks.with(event.aggregate_id.clone(), || self.replay_event(definition, &versioned, event, &options))
    }

    pub fn project(&self, versioned_type: &str, projector: ProjectorRegistration) {
        let mut projectors = lock(&self.projectors);
        projectors.entry(versioned_type.to_string()).or_default().push(Arc::from(projector));
    }

    pub fn projector_registrations(&self) -> Vec<(String, usize)> {
        lock(&self.projectors).iter().map(|(r#type, projectors)| (r#type.clone(), projectors.len())).collect()
    }

    pub fn listen(&self, listener: ListenerRegistration) -> ListenerHandle {
        let id = self.next_listener_id.fetch_add(1, Ordering::Relaxed);
        lock(&self.listeners).push((id, Arc::from(listener)));
        ListenerHandle { listeners: self.listeners.clone(), id }
    }

    pub fn subscribe_all(&self, location: Option<LocationRef>) -> Subscription {
        self.register(None, location)
    }

    pub fn subscribe(&self, types: &[&str], location: Option<LocationRef>) -> Subscription {
        self.register(Some(types.iter().map(|r#type| r#type.to_string()).collect()), location)
    }

    pub fn remove(&self, aggregate_id: &str) -> Result<(), BusError> {
        self.db.transaction(TransactionBehavior::Immediate, |tx| {
            tx.execute("DELETE FROM event_sequence WHERE aggregate_id = ?1", [aggregate_id])?;
            tx.execute("DELETE FROM event WHERE aggregate_id = ?1", [aggregate_id])?;
            Ok::<(), BusError>(())
        })?;
        lock(&self.routes).remove(aggregate_id);
        Ok(())
    }

    pub fn claim(&self, aggregate_id: &str, owner_id: &str) -> Result<(), BusError> {
        self.db.read(|connection| {
            connection.execute(
                "UPDATE event_sequence SET owner_id = ?2 WHERE aggregate_id = ?1",
                params![aggregate_id, owner_id],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    fn commit_one(
        &self,
        r#type: &str,
        spec: DurableSpec,
        aggregate_id: &str,
        event: Event,
        commit: Option<CommitHook>,
    ) -> Result<Event, BusError> {
        let persist = self.options.persist;
        let versioned = versioned_type(r#type, spec.version);
        let mut routes = lock(&self.routes);
        let (committed, pending) = self.db.transaction(TransactionBehavior::Immediate, |tx| {
            let seq = latest_sequence(tx, aggregate_id)? + 1;
            if persist && let Some((existing_aggregate, existing_seq)) = stored_event(tx, event.id())? {
                return Err(already_exists(r#type, event.id(), &existing_aggregate, existing_seq));
            }
            let committed = with_durable(&event, aggregate_id, seq, spec.version);
            let pending = routes.prepare(tx, &[&committed])?;
            self.run_projectors(&versioned, tx, &committed)?;
            if let Some(commit) = commit {
                commit(tx, seq)?;
            }
            reserve_sequence(tx, aggregate_id, seq)?;
            if persist {
                insert_event(tx, &committed, aggregate_id, seq, &versioned)?;
            }
            Ok((committed, pending))
        })?;
        pending.apply(&mut routes);
        let resolved = pending.routes(0);
        drop(routes);
        self.wake(aggregate_id);
        self.notify(&committed, resolved, true)?;
        Ok(committed)
    }

    fn commit_batch(&self, aggregate_id: &str, prepared: Vec<Prepared>) -> Result<Vec<Event>, BusError> {
        let persist = self.options.persist;
        let mut routes = lock(&self.routes);
        let (committed, pending) = self.db.transaction(TransactionBehavior::Immediate, |tx| {
            let first = latest_sequence(tx, aggregate_id)? + 1;
            let final_seq = first + prepared.len() as i64 - 1;
            let queued = prepared
                .iter()
                .enumerate()
                .map(|(index, item)| with_durable(&item.event, aggregate_id, first + index as i64, item.spec.version))
                .collect::<Vec<_>>();
            let pending = routes.prepare(tx, &queued.iter().collect::<Vec<_>>())?;
            let mut ids: Vec<String> = Vec::new();
            let mut rows = Vec::new();
            for (index, item) in prepared.into_iter().enumerate() {
                let seq = first + index as i64;
                if persist {
                    if ids.iter().any(|id| id == item.event.id()) {
                        return Err(BusError::InvalidDurableEvent {
                            r#type: item.r#type.to_string(),
                            message: format!("Event {} appears more than once in the batch", item.event.id()),
                        });
                    }
                    ids.push(item.event.id().to_string());
                    if let Some((existing_aggregate, existing_seq)) = stored_event(tx, item.event.id())? {
                        return Err(already_exists(item.r#type, item.event.id(), &existing_aggregate, existing_seq));
                    }
                }
                let versioned = versioned_type(item.r#type, item.spec.version);
                self.run_projectors(&versioned, tx, &queued[index])?;
                if let Some(commit) = item.commit {
                    commit(tx, seq)?;
                }
                if persist {
                    rows.push(EventRow {
                        id: queued[index].id().to_string(),
                        seq,
                        created: queued[index].created(),
                        r#type: versioned,
                        data: encode_json(queued[index].data()),
                    });
                }
            }
            tx.execute(
                "INSERT INTO event_sequence (aggregate_id, seq) VALUES (?1, ?2) \
                 ON CONFLICT(aggregate_id) DO UPDATE SET seq = excluded.seq",
                params![aggregate_id, final_seq],
            )?;
            let mut insert_statement = tx.prepare_cached(
                "INSERT INTO event (id, aggregate_id, seq, created, type, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for row in rows {
                insert_statement.execute(params![row.id, aggregate_id, row.seq, row.created, row.r#type, row.data])?;
            }
            Ok((queued, pending))
        })?;
        pending.apply(&mut routes);
        let resolutions = (0..committed.len()).map(|index| pending.routes(index)).collect::<Vec<_>>();
        drop(routes);
        self.wake(aggregate_id);
        for (event, resolved) in committed.iter().zip(resolutions) {
            self.notify(event, resolved, true)?;
        }
        Ok(committed)
    }

    fn replay_event(
        &self,
        definition: Definition,
        versioned: &str,
        event: SerializedEvent,
        options: &ReplayOptions,
    ) -> Result<(), BusError> {
        let spec = definition.durable.expect("a resolved durable definition carries its spec");
        let persist = self.options.persist;
        let data = decode::decode_stored(&event.r#type, &event.data)?;
        let aggregate = aggregate_id(definition.r#type, spec.aggregate, &data)?;
        if aggregate != event.aggregate_id {
            return Err(BusError::InvalidDurableEvent {
                r#type: definition.r#type.to_string(),
                message: format!("Aggregate mismatch: expected {}, got {aggregate}", event.aggregate_id),
            });
        }
        let created = event.created.unwrap_or(0);
        let mut payload = Map::new();
        payload.insert("id".to_string(), Value::String(event.id.clone()));
        payload.insert("created".to_string(), Value::from(created));
        payload.insert("type".to_string(), Value::String(definition.r#type.to_string()));
        payload.insert("data".to_string(), data.clone());
        let payload = Event::from_payload(payload);
        let mut routes = lock(&self.routes);
        let committed = self.db.transaction(TransactionBehavior::Immediate, |tx| {
            let row = sequence_row(tx, &aggregate)?;
            let latest = row.as_ref().map_or(-1, |(seq, _)| *seq);
            let owner = row.as_ref().and_then(|(_, owner)| owner.clone());
            if options.strict_owner
                && let Some(owner_id) = &owner
                && options.owner_id.as_ref() != Some(owner_id)
            {
                return Err(BusError::InvalidDurableEvent {
                    r#type: definition.r#type.to_string(),
                    message: format!(
                        "Replay owner mismatch for aggregate {aggregate}: expected {owner_id}, got {}",
                        options.owner_id.as_deref().unwrap_or("none")
                    ),
                });
            }
            if event.seq <= latest {
                if !persist {
                    return Ok(None);
                }
                if replay_row_matches(tx, versioned, definition.r#type, &event, &data, created)? {
                    if let Some(owner_id) = &options.owner_id
                        && owner.is_none()
                    {
                        tx.execute(
                            "UPDATE event_sequence SET owner_id = ?2 WHERE aggregate_id = ?1",
                            params![aggregate, owner_id],
                        )?;
                    }
                    return Ok(None);
                }
                return Err(BusError::InvalidDurableEvent {
                    r#type: definition.r#type.to_string(),
                    message: format!("Replay diverged at aggregate {aggregate} sequence {}", event.seq),
                });
            }
            if let Some(owner_id) = &owner
                && options.owner_id.as_ref() != Some(owner_id)
            {
                return Ok(None);
            }
            if event.seq != latest + 1 {
                return Err(BusError::InvalidDurableEvent {
                    r#type: definition.r#type.to_string(),
                    message: format!(
                        "Sequence mismatch for aggregate {aggregate}: expected {}, got {}",
                        latest + 1,
                        event.seq
                    ),
                });
            }
            if persist && let Some((existing_aggregate, existing_seq)) = stored_event(tx, &event.id)? {
                return Err(already_exists(definition.r#type, &event.id, &existing_aggregate, existing_seq));
            }
            let committed = with_durable(&payload, &aggregate, event.seq, spec.version);
            let pending = routes.prepare(tx, &[&committed])?;
            self.run_projectors(versioned, tx, &committed)?;
            if row.is_none() {
                tx.execute(
                    "INSERT INTO event_sequence (aggregate_id, seq, owner_id) VALUES (?1, ?2, ?3)",
                    params![aggregate, event.seq, options.owner_id.as_deref()],
                )?;
            } else {
                reserve_sequence(tx, &aggregate, event.seq)?;
                if let Some(owner_id) = &options.owner_id
                    && owner.is_none()
                {
                    tx.execute(
                        "UPDATE event_sequence SET owner_id = ?2 WHERE aggregate_id = ?1",
                        params![aggregate, owner_id],
                    )?;
                }
            }
            if persist {
                insert_event(tx, &committed, &aggregate, event.seq, versioned)?;
            }
            Ok(Some((committed, pending)))
        })?;
        let Some((committed, pending)) = committed else {
            return Ok(());
        };
        pending.apply(&mut routes);
        let resolved = pending.routes(0);
        drop(routes);
        self.wake(&aggregate);
        if options.publish {
            self.notify(&committed, resolved, true)?;
        }
        Ok(())
    }

    fn make_payload(&self, definition: &Definition, data: Value, options: &PublishOptions) -> Event {
        let created = self.options.now.as_ref().map_or_else(opencode_storage::id::now_ms, |now| now());
        let mut payload = Map::new();
        payload.insert("id".to_string(), Value::String(options.id.clone().unwrap_or_else(EventId::create)));
        payload.insert("created".to_string(), Value::from(created));
        if let Some(metadata) = &options.metadata {
            payload.insert("metadata".to_string(), Value::Object(metadata.clone()));
        }
        payload.insert("type".to_string(), Value::String(definition.r#type.to_string()));
        if !options.global
            && let Some(location) = &options.location
        {
            payload.insert("location".to_string(), location_value(location));
        }
        payload.insert("data".to_string(), data);
        Event::from_payload(payload)
    }

    fn notify(&self, event: &Event, routes: Option<Arc<[LocationRef]>>, isolate: bool) -> Result<(), BusError> {
        let listeners = lock(&self.listeners).iter().map(|(_, listener)| listener.clone()).collect::<Vec<_>>();
        for listener in listeners {
            let result = listener(event);
            if !isolate {
                result?;
                continue;
            }
            if let Err(error) = result {
                eprintln!("Event listener failed: eventID={} eventType={} error={error}", event.id(), event.r#type());
            }
        }
        let event = Arc::new(event.clone());
        let (untyped, typed) = {
            let subscribers = lock(&self.subscribers);
            let untyped = subscribers
                .iter()
                .filter(|subscriber| subscriber.types.is_none())
                .map(|subscriber| subscriber.sender.clone())
                .collect::<Vec<_>>();
            let typed = subscribers
                .iter()
                .filter(|subscriber| {
                    subscriber.types.as_ref().is_some_and(|types| types.iter().any(|r#type| r#type == event.r#type()))
                })
                .map(|subscriber| subscriber.sender.clone())
                .collect::<Vec<_>>();
            (untyped, typed)
        };
        for sender in untyped.into_iter().chain(typed) {
            let _ = sender.send((event.clone(), routes.clone()));
        }
        Ok(())
    }

    fn register(&self, types: Option<Vec<String>>, location: Option<LocationRef>) -> Subscription {
        let (sender, receiver) = unbounded_channel();
        let id = self.next_subscriber_id.fetch_add(1, Ordering::Relaxed);
        lock(&self.subscribers).push(Subscriber { id, types, sender });
        Subscription { receiver, subscribers: self.subscribers.clone(), id, location }
    }

    fn run_projectors(&self, key: &str, connection: &Connection, event: &Event) -> Result<(), BusError> {
        let projectors = lock(&self.projectors)
            .get(key)
            .map(|projectors| projectors.iter().map(Arc::clone).collect::<Vec<_>>())
            .unwrap_or_default();
        for projector in projectors {
            projector(connection, event)?;
        }
        Ok(())
    }

    fn wake(&self, aggregate_id: &str) {
        let notifies = lock(&self.durable).get(aggregate_id).cloned().unwrap_or_default();
        for notify in notifies {
            notify.notify_one();
        }
    }
}

pub fn latest_sequence(connection: &Connection, aggregate_id: &str) -> rusqlite::Result<i64> {
    let seq = connection
        .prepare_cached("SELECT seq FROM event_sequence WHERE aggregate_id = ?1")?
        .query_row([aggregate_id], |row| row.get(0))
        .optional()?;
    Ok(seq.unwrap_or(-1))
}

pub fn reserve_sequence(connection: &Connection, aggregate_id: &str, seq: i64) -> rusqlite::Result<()> {
    connection
        .prepare_cached(
            "INSERT INTO event_sequence (aggregate_id, seq) VALUES (?1, ?2) \
             ON CONFLICT(aggregate_id) DO UPDATE SET seq = max(seq, excluded.seq)",
        )?
        .execute(params![aggregate_id, seq])?;
    Ok(())
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

fn aggregate_id(r#type: &str, aggregate: &str, data: &Value) -> Result<String, BusError> {
    data.get(aggregate).and_then(Value::as_str).map(str::to_string).ok_or_else(|| BusError::InvalidDurableEvent {
        r#type: r#type.to_string(),
        message: format!("Expected string aggregate field {aggregate}"),
    })
}

fn already_exists(r#type: &str, id: &str, aggregate_id: &str, seq: i64) -> BusError {
    BusError::InvalidDurableEvent {
        r#type: r#type.to_string(),
        message: format!("Event {id} already exists at aggregate {aggregate_id} sequence {seq}"),
    }
}

fn stored_event(connection: &Connection, id: &str) -> rusqlite::Result<Option<(String, i64)>> {
    connection
        .prepare_cached("SELECT aggregate_id, seq FROM event WHERE id = ?1")?
        .query_row([id], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()
}

struct StoredRow {
    id: String,
    r#type: String,
    created: i64,
    data: String,
}

fn sequence_row(connection: &Connection, aggregate_id: &str) -> rusqlite::Result<Option<(i64, Option<String>)>> {
    connection
        .query_row("SELECT seq, owner_id FROM event_sequence WHERE aggregate_id = ?1", [aggregate_id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()
}

fn stored_row(connection: &Connection, aggregate_id: &str, seq: i64) -> rusqlite::Result<Option<StoredRow>> {
    connection
        .query_row(
            "SELECT id, type, created, data FROM event WHERE aggregate_id = ?1 AND seq = ?2",
            params![aggregate_id, seq],
            |row| Ok(StoredRow { id: row.get(0)?, r#type: row.get(1)?, created: row.get(2)?, data: row.get(3)? }),
        )
        .optional()
}

fn replay_row_matches(
    connection: &Connection,
    versioned: &str,
    r#type: &str,
    event: &SerializedEvent,
    data: &Value,
    created: i64,
) -> Result<bool, BusError> {
    let Some(stored) = stored_row(connection, &event.aggregate_id, event.seq)? else {
        return Ok(false);
    };
    if stored.id != event.id || stored.r#type != versioned || stored.created != created {
        return Ok(false);
    }
    let parsed = serde_json::from_str::<Value>(&stored.data)
        .map_err(|error| BusError::InvalidDurableEvent { r#type: versioned.to_string(), message: error.to_string() })?;
    Ok(&legacy::upgrade_event(r#type, parsed) == data)
}

fn insert_event(
    connection: &Connection,
    event: &Event,
    aggregate_id: &str,
    seq: i64,
    versioned: &str,
) -> rusqlite::Result<()> {
    connection
        .prepare_cached(
            "INSERT INTO event (id, aggregate_id, seq, created, type, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?
        .execute(params![event.id(), aggregate_id, seq, event.created(), versioned, encode_json(event.data())])?;
    Ok(())
}

fn with_durable(event: &Event, aggregate_id: &str, seq: i64, version: u32) -> Event {
    let mut payload = event.payload().clone();
    let mut durable = Map::new();
    durable.insert("aggregateID".to_string(), Value::String(aggregate_id.to_string()));
    durable.insert("seq".to_string(), Value::from(seq));
    durable.insert("version".to_string(), Value::from(version));
    payload.insert("durable".to_string(), Value::Object(durable));
    Event::from_payload(payload)
}

fn location_value(location: &LocationRef) -> Value {
    let mut object = Map::new();
    object.insert("directory".to_string(), Value::String(location.directory.0.clone()));
    if let Some(workspace_id) = &location.workspace_id {
        object.insert("workspaceID".to_string(), Value::String(workspace_id.0.clone()));
    }
    Value::Object(object)
}

fn accepts(location: Option<&LocationRef>, event: &Event, routes: Option<&[LocationRef]>) -> bool {
    let Some(location) = location else {
        return true;
    };
    if let Some(routes) = routes {
        return routes.contains(location);
    }
    let Some(event_location) = event.location() else {
        return true;
    };
    &event_location == location
}
