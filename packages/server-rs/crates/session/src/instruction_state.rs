use opencode_bus::{Bus, BusError, Definition, PublishOptions};
use opencode_contract::codec::{JsonMap, encode_json};
use opencode_contract::generated::{
    InstructionHash, LocationRef, SessionID, SessionInstructionsUpdatedData, SessionInstructionsUpdatedDataDeltaValue,
    SessionInstructionsUpdatedDataDeltaValueString,
};
use opencode_storage::Database;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value, json};

use crate::instruction::decode_values;
use crate::instructions::{self, Rendered, Source, Values};

pub struct Observation {
    pub session_id: String,
    pub initial: bool,
    pub previous: Values,
    pub current: Values,
    pub delta: Vec<(String, Option<String>)>,
    pub blobs: Vec<(String, Value)>,
}

struct State {
    initial_values: Values,
    current_values: Values,
}

pub fn observe(db: &Database, list: &[Source], session_id: &str) -> Result<Observation, BusError> {
    let observed = instructions::read(list);
    let stored = find(db, session_id)?;
    let previous = stored.as_ref().map_or_else(Vec::new, |state| state.current_values.clone());
    let admission =
        instructions::diff(&observed, stored.as_ref().map(|state| state.current_values.as_slice())).map_err(blocked)?;
    let current = instructions::apply_hash_delta(&previous, &admission.delta);
    Ok(Observation {
        session_id: session_id.to_string(),
        initial: stored.is_none(),
        previous,
        current,
        delta: admission.delta,
        blobs: admission.blobs,
    })
}

pub fn commit(db: &Database, bus: &Bus, list: &[Source], observation: &Observation) -> Result<(), BusError> {
    commit_at(db, bus, list, observation, None)
}

pub fn commit_at(
    db: &Database,
    bus: &Bus,
    list: &[Source],
    observation: &Observation,
    location: Option<LocationRef>,
) -> Result<(), BusError> {
    if !observation.initial && observation.delta.is_empty() {
        return Ok(());
    }
    let text = if observation.initial { String::new() } else { render_update_text(db, list, observation)? };
    let data = SessionInstructionsUpdatedData {
        session_id: SessionID(observation.session_id.clone()),
        delta: JsonMap(
            observation
                .delta
                .iter()
                .map(|(key, hash)| {
                    let value = match hash {
                        Some(hash) => SessionInstructionsUpdatedDataDeltaValue::String(InstructionHash(hash.clone())),
                        None => SessionInstructionsUpdatedDataDeltaValue::String2(
                            SessionInstructionsUpdatedDataDeltaValueString::Removed,
                        ),
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        text: (!text.is_empty()).then_some(text),
    };
    let definition =
        Definition::latest_durable("session.instructions.updated").expect("session.instructions.updated is durable");
    let value = serde_json::from_str::<Value>(&encode_json(&data)).expect("the encoded instruction event is JSON");
    let metadata = observation.initial.then(|| {
        let mut metadata = Map::new();
        metadata.insert("instructions".to_string(), json!({ "initial": true }));
        metadata
    });
    let blobs = observation.blobs.clone();
    let options = PublishOptions {
        location,
        metadata,
        commit: Some(Box::new(move |connection: &Connection, _seq: i64| {
            for (hash, value) in &blobs {
                connection.execute(
                    "INSERT INTO instruction_blob (hash, value) VALUES (?1, ?2) ON CONFLICT(hash) DO NOTHING",
                    params![hash, encode_json(value)],
                )?;
            }
            Ok(())
        })),
        ..Default::default()
    };
    bus.publish(&definition, value, options)?;
    Ok(())
}

pub fn prepare(db: &Database, bus: &Bus, list: &[Source], session_id: &str) -> Result<(), BusError> {
    prepare_at(db, bus, list, session_id, None)
}

pub fn prepare_at(
    db: &Database,
    bus: &Bus,
    list: &[Source],
    session_id: &str,
    location: Option<LocationRef>,
) -> Result<(), BusError> {
    commit_at(db, bus, list, &observe(db, list, session_id)?, location)
}

pub fn initial(db: &Database, session_id: &str, list: &[Source]) -> Result<Vec<Rendered>, BusError> {
    let state = find(db, session_id)?.ok_or_else(|| missing_state(session_id))?;
    let hashes = state.initial_values.iter().map(|(_, hash)| hash.clone()).collect::<Vec<_>>();
    let blobs = load_blobs(db, &hashes)?;
    Ok(instructions::render_initial(list, &dereference(&state.initial_values, &blobs)?))
}

fn find(db: &Database, session_id: &str) -> Result<Option<State>, BusError> {
    let row = db.read(|connection| {
        Ok(connection
            .query_row(
                "SELECT initial_values, current_values FROM instruction_state WHERE session_id = ?1",
                [session_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?)
    })?;
    row.map(|(initial_values, current_values)| {
        Ok(State { initial_values: decode_values(&initial_values)?, current_values: decode_values(&current_values)? })
    })
    .transpose()
}

fn render_update_text(db: &Database, list: &[Source], observation: &Observation) -> Result<String, BusError> {
    let replaced = observation
        .previous
        .iter()
        .filter(|(key, _)| observation.delta.iter().any(|(changed, _)| changed == key))
        .map(|(_, hash)| hash.clone())
        .collect::<Vec<_>>();
    let blobs = load_blobs(db, &replaced)?;
    let previous = observation
        .previous
        .iter()
        .filter(|(key, _)| observation.delta.iter().any(|(changed, _)| changed == key))
        .map(|(key, hash)| {
            let value = blobs
                .iter()
                .find(|(candidate, _)| candidate == hash)
                .map(|(_, value)| value.clone())
                .ok_or_else(|| missing_blob(hash))?;
            Ok((key.clone(), value))
        })
        .collect::<Result<Vec<_>, BusError>>()?;
    let delta = dereference_delta(&observation.delta, &observation.blobs)?;
    Ok(instructions::render_update(list, &previous, &delta))
}

fn load_blobs(db: &Database, hashes: &[String]) -> Result<Vec<(String, Value)>, BusError> {
    hashes
        .iter()
        .map(|hash| {
            let text = db.read(|connection| {
                Ok(connection
                    .query_row("SELECT value FROM instruction_blob WHERE hash = ?1", [hash], |row| {
                        row.get::<_, String>(0)
                    })
                    .optional()?)
            })?;
            let text = text.ok_or_else(|| missing_blob(hash))?;
            let value = serde_json::from_str(&text).map_err(|error| invalid(error.to_string()))?;
            Ok((hash.clone(), value))
        })
        .collect()
}

fn dereference(values: &Values, blobs: &[(String, Value)]) -> Result<Vec<(String, Value)>, BusError> {
    values
        .iter()
        .map(|(key, hash)| {
            let value = blobs
                .iter()
                .find(|(candidate, _)| candidate == hash)
                .map(|(_, value)| value.clone())
                .ok_or_else(|| missing_blob(hash))?;
            Ok((key.clone(), value))
        })
        .collect()
}

fn dereference_delta(
    delta: &[(String, Option<String>)],
    blobs: &[(String, Value)],
) -> Result<Vec<(String, Option<Value>)>, BusError> {
    delta
        .iter()
        .map(|(key, hash)| {
            let value = match hash {
                Some(hash) => Some(
                    blobs
                        .iter()
                        .find(|(candidate, _)| candidate == hash)
                        .map(|(_, value)| value.clone())
                        .ok_or_else(|| missing_blob(hash))?,
                ),
                None => None,
            };
            Ok((key.clone(), value))
        })
        .collect()
}

fn blocked(error: instructions::InitializationBlocked) -> BusError {
    BusError::InvalidDurableEvent { r#type: "instruction_state".to_string(), message: error.to_string() }
}

fn missing_state(session_id: &str) -> BusError {
    BusError::InvalidDurableEvent {
        r#type: "instruction_state".to_string(),
        message: format!("Instruction state not found during assembly: {session_id}"),
    }
}

fn missing_blob(hash: &str) -> BusError {
    BusError::InvalidDurableEvent {
        r#type: "instruction_state".to_string(),
        message: format!("Instruction blob not found: {hash}"),
    }
}

fn invalid(message: String) -> BusError {
    BusError::InvalidDurableEvent { r#type: "instruction_state".to_string(), message }
}
