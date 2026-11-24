use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use futures_util::stream::unfold;
use rusqlite::params;
use serde_json::{Map, Value};
use tokio::sync::Notify;

use crate::bus::{Bus, latest_sequence, lock};
use crate::decode;
use crate::error::BusError;
use crate::event::{Definition, Event};

pub struct LogInput {
    pub aggregate_id: String,
    pub after: Option<i64>,
    pub follow: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LogItem {
    Event(Event),
    Synced { aggregate_id: String, seq: Option<i64> },
}

impl LogItem {
    pub fn is_synced(&self) -> bool {
        matches!(self, LogItem::Synced { .. })
    }

    pub fn json(&self) -> String {
        let payload = match self {
            LogItem::Event(event) => event.payload().clone(),
            LogItem::Synced { aggregate_id, seq } => {
                let mut payload = Map::new();
                payload.insert("type".to_string(), Value::String("log.synced".to_string()));
                payload.insert("aggregateID".to_string(), Value::String(aggregate_id.clone()));
                if let Some(seq) = seq {
                    payload.insert("seq".to_string(), Value::from(*seq));
                }
                payload
            }
        };
        opencode_contract::codec::encode_json(&Value::Object(payload))
    }
}

pub(crate) struct Wake {
    durable: Arc<Mutex<HashMap<String, Vec<Arc<Notify>>>>>,
    aggregate_id: String,
    notify: Arc<Notify>,
}

impl Wake {
    fn register(durable: Arc<Mutex<HashMap<String, Vec<Arc<Notify>>>>>, aggregate_id: String) -> Wake {
        let notify = Arc::new(Notify::new());
        lock(&durable).entry(aggregate_id.clone()).or_default().push(notify.clone());
        Wake { durable, aggregate_id, notify }
    }

    async fn notified(&self) {
        self.notify.notified().await;
    }
}

impl Drop for Wake {
    fn drop(&mut self) {
        let mut durable = lock(&self.durable);
        let Some(notifies) = durable.get_mut(&self.aggregate_id) else {
            return;
        };
        notifies.retain(|notify| !Arc::ptr_eq(notify, &self.notify));
        let empty = notifies.is_empty();
        if empty {
            durable.remove(&self.aggregate_id);
        }
    }
}

struct LogPage {
    seq: Option<i64>,
    items: Vec<LogItem>,
}

struct LogRow {
    id: String,
    seq: i64,
    created: i64,
    r#type: String,
    data: String,
}

struct LogState {
    bus: Arc<Bus>,
    input: LogInput,
    wake: Option<Wake>,
    cursor: i64,
    target: i64,
    captured: bool,
    marked: bool,
    pending: VecDeque<LogItem>,
    done: bool,
}

impl LogState {
    async fn next(&mut self) -> Option<Result<LogItem, BusError>> {
        match self.advance().await {
            Ok(Some(item)) => Some(Ok(item)),
            Ok(None) => None,
            Err(error) => {
                self.done = true;
                Some(Err(error))
            }
        }
    }

    async fn advance(&mut self) -> Result<Option<LogItem>, BusError> {
        loop {
            if let Some(item) = self.pending.pop_front() {
                return Ok(Some(item));
            }
            if self.done {
                return Ok(None);
            }
            if !self.captured {
                if self.input.follow {
                    self.wake = Some(Wake::register(Arc::clone(&self.bus.durable), self.input.aggregate_id.clone()));
                }
                self.target = latest_sequence_of(&self.bus, &self.input.aggregate_id)?;
                self.captured = true;
                continue;
            }
            if self.cursor < self.target {
                let page = self.bus.read_log_page(&self.input.aggregate_id, self.cursor, self.target)?;
                self.cursor = page.seq.unwrap_or(self.target);
                self.pending.extend(page.items);
                continue;
            }
            if !self.marked {
                self.marked = true;
                return Ok(Some(LogItem::Synced {
                    aggregate_id: self.input.aggregate_id.clone(),
                    seq: (self.target >= 0).then_some(self.target),
                }));
            }
            if !self.input.follow {
                self.done = true;
                return Ok(None);
            }
            let latest = latest_sequence_of(&self.bus, &self.input.aggregate_id)?;
            if latest > self.cursor {
                self.target = latest;
                continue;
            }
            let Some(wake) = &self.wake else {
                self.done = true;
                return Ok(None);
            };
            wake.notified().await;
        }
    }
}

impl Bus {
    pub fn log(
        self: &Arc<Self>,
        input: LogInput,
    ) -> impl futures_util::Stream<Item = Result<LogItem, BusError>> + use<> {
        let cursor = input.after.unwrap_or(-1);
        let state = LogState {
            bus: Arc::clone(self),
            input,
            wake: None,
            cursor,
            target: -1,
            captured: false,
            marked: false,
            pending: VecDeque::new(),
            done: false,
        };
        unfold(state, |mut state| async move { state.next().await.map(|item| (item, state)) })
    }

    fn read_log_page(&self, aggregate_id: &str, after: i64, through: i64) -> Result<LogPage, BusError> {
        if let Some(before) = &self.options.before_aggregate_read {
            before(aggregate_id);
        }
        let limit = i64::try_from(self.options.log_read_page_size).unwrap_or(i64::MAX);
        let rows = self.db.read(|connection| {
            let mut statement = connection.prepare_cached(
                "SELECT id, seq, created, type, data FROM event \
                 WHERE aggregate_id = ?1 AND seq > ?2 AND seq <= ?3 ORDER BY seq LIMIT ?4",
            )?;
            let rows = statement.query_map(params![aggregate_id, after, through, limit], |row| {
                Ok(LogRow {
                    id: row.get(0)?,
                    seq: row.get(1)?,
                    created: row.get(2)?,
                    r#type: row.get(3)?,
                    data: row.get(4)?,
                })
            })?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })?;
        let seq = rows.last().map(|row| row.seq);
        let items = rows
            .into_iter()
            .map(|row| decode_row(aggregate_id, row))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect();
        Ok(LogPage { seq, items })
    }
}

fn latest_sequence_of(bus: &Bus, aggregate_id: &str) -> Result<i64, BusError> {
    Ok(bus.db.read(|connection| Ok(latest_sequence(connection, aggregate_id)?))?)
}

fn decode_row(aggregate_id: &str, row: LogRow) -> Result<Option<LogItem>, BusError> {
    let Some(definition) = Definition::durable_lookup(&row.r#type) else {
        return Ok(None);
    };
    let spec = definition.durable.expect("a resolved durable definition carries its spec");
    let parsed = serde_json::from_str::<Value>(&row.data)
        .map_err(|error| BusError::InvalidDurableEvent { r#type: row.r#type.clone(), message: error.to_string() })?;
    let data = decode::decode_stored(&row.r#type, &parsed)?;
    let mut durable = Map::new();
    durable.insert("aggregateID".to_string(), Value::String(aggregate_id.to_string()));
    durable.insert("seq".to_string(), Value::from(row.seq));
    durable.insert("version".to_string(), Value::from(spec.version));
    let mut payload = Map::new();
    payload.insert("id".to_string(), Value::String(row.id));
    payload.insert("created".to_string(), Value::from(row.created));
    payload.insert("type".to_string(), Value::String(definition.r#type.to_string()));
    payload.insert("durable".to_string(), Value::Object(durable));
    payload.insert("data".to_string(), data);
    Ok(Some(LogItem::Event(Event::from_payload(payload))))
}
