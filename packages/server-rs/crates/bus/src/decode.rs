use opencode_contract::codec::{Decode, Path, encode_json};
use opencode_contract::generated::DurableEvent;
use serde_json::{Map, Value};

use crate::error::BusError;
use crate::event::Definition;
use crate::legacy;

pub fn decode_stored(versioned_type: &str, data: &Value) -> Result<Value, BusError> {
    let Some(definition) = Definition::durable_lookup(versioned_type) else {
        return Err(BusError::unknown_durable_event(versioned_type));
    };
    let spec = definition.durable.expect("a resolved durable definition carries its spec");
    let mut envelope = Map::new();
    envelope.insert("id".to_string(), Value::String("evt_replay".to_string()));
    envelope.insert("created".to_string(), Value::from(0));
    envelope.insert("type".to_string(), Value::String(definition.r#type.to_string()));
    let mut durable = Map::new();
    durable.insert("aggregateID".to_string(), Value::String(String::new()));
    durable.insert("seq".to_string(), Value::from(0));
    durable.insert("version".to_string(), Value::from(spec.version));
    envelope.insert("durable".to_string(), Value::Object(durable));
    envelope.insert("data".to_string(), legacy::upgrade_event(definition.r#type, data.clone()));
    let event = DurableEvent::decode(&Value::Object(envelope), &mut Path::new())
        .map_err(|error| invalid(versioned_type, error))?;
    let mut encoded: Map<String, Value> =
        serde_json::from_str(&encode_json(&event)).map_err(|error| invalid(versioned_type, error))?;
    Ok(encoded.remove("data").unwrap_or(Value::Null))
}

fn invalid(versioned_type: &str, error: impl std::fmt::Display) -> BusError {
    BusError::InvalidDurableEvent { r#type: versioned_type.to_string(), message: error.to_string() }
}
