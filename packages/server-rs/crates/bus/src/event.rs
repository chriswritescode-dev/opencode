use opencode_contract::generated::{AbsolutePath, LocationRef, WorkspaceID};
use serde_json::{Map, Value};

pub struct EventId;

impl EventId {
    pub fn create() -> String {
        format!("evt_{}", opencode_storage::id::ascending())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DurableSpec {
    pub version: u32,
    pub aggregate: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Definition {
    pub r#type: &'static str,
    pub durable: Option<DurableSpec>,
}

impl Definition {
    pub fn ephemeral(r#type: &'static str) -> Definition {
        Definition { r#type, durable: None }
    }

    pub fn durable(r#type: &'static str, version: u32, aggregate: &'static str) -> Definition {
        Definition { r#type, durable: Some(DurableSpec { version, aggregate }) }
    }

    pub fn durable_lookup(versioned_type: &str) -> Option<Definition> {
        opencode_contract::generated::DURABLE_EVENTS
            .iter()
            .copied()
            .find(|(r#type, version, _)| format!("{type}.{version}") == versioned_type)
            .map(|(r#type, version, aggregate)| Definition::durable(r#type, version, aggregate))
    }
}

pub fn versioned_type(r#type: &str, version: u32) -> String {
    format!("{type}.{version}")
}

pub(crate) fn parse_location(value: &Value) -> Option<LocationRef> {
    let object = value.as_object()?;
    let directory = object.get("directory")?.as_str()?;
    let workspace_id = match object.get("workspaceID") {
        None | Some(Value::Null) => None,
        Some(value) => Some(WorkspaceID(value.as_str()?.to_string())),
    };
    Some(LocationRef { directory: AbsolutePath(directory.to_string()), workspace_id })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableEnvelope {
    pub aggregate_id: String,
    pub seq: i64,
    pub version: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    payload: Map<String, Value>,
}

impl Event {
    pub fn from_payload(payload: Map<String, Value>) -> Event {
        Event { payload }
    }

    pub fn payload(&self) -> &Map<String, Value> {
        &self.payload
    }

    pub fn into_payload(self) -> Map<String, Value> {
        self.payload
    }

    pub fn id(&self) -> &str {
        self.payload.get("id").and_then(Value::as_str).expect("an event carries an id")
    }

    pub fn r#type(&self) -> &str {
        self.payload.get("type").and_then(Value::as_str).expect("an event carries a type")
    }

    pub fn created(&self) -> i64 {
        self.payload.get("created").and_then(Value::as_i64).expect("an event carries a creation time")
    }

    pub fn data(&self) -> &Value {
        self.payload.get("data").expect("an event carries data")
    }

    pub fn location(&self) -> Option<LocationRef> {
        parse_location(self.payload.get("location")?)
    }

    pub fn durable(&self) -> Option<DurableEnvelope> {
        let object = self.payload.get("durable")?.as_object()?;
        Some(DurableEnvelope {
            aggregate_id: object.get("aggregateID")?.as_str()?.to_string(),
            seq: object.get("seq")?.as_i64()?,
            version: u32::try_from(object.get("version")?.as_u64()?).ok()?,
        })
    }
}
