use opencode_bus::BusError;
use opencode_contract::codec::encode_json;
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension, params_from_iter};
use serde_json::{Map, Value};

const ENTRY_INSERT_BATCH: usize = 10;

pub(crate) fn apply(
    connection: &Connection,
    session_id: &str,
    seq: i64,
    delta: &[(String, Option<String>)],
) -> Result<(), BusError> {
    let stored = connection
        .query_row("SELECT current_values FROM instruction_state WHERE session_id = ?1", [session_id], |row| {
            row.get::<_, String>(0)
        })
        .optional()?;
    let Some(stored) = stored else {
        return initialize(connection, session_id, seq, &apply_hash_delta(Vec::new(), delta));
    };
    let current = apply_hash_delta(decode_values(&stored)?, delta);
    connection.execute(
        "UPDATE instruction_state SET through_seq = ?1, current_values = ?2 WHERE session_id = ?3",
        rusqlite::params![seq, encode_values(&current), session_id],
    )?;
    Ok(())
}

pub(crate) fn initialize(
    connection: &Connection,
    session_id: &str,
    seq: i64,
    values: &[(String, String)],
) -> Result<(), BusError> {
    let values = encode_values(values);
    connection.execute(
        "INSERT INTO instruction_state (session_id, epoch_start, through_seq, initial_values, current_values) \
         VALUES (?1, ?2, ?2, ?3, ?3) ON CONFLICT DO NOTHING",
        rusqlite::params![session_id, seq, values],
    )?;
    Ok(())
}

pub(crate) fn advance_epoch(connection: &Connection, session_id: &str, epoch_start: i64) -> Result<(), BusError> {
    connection.execute(
        "UPDATE instruction_state SET epoch_start = ?1, through_seq = ?1, initial_values = current_values \
         WHERE session_id = ?2",
        rusqlite::params![epoch_start, session_id],
    )?;
    Ok(())
}

#[allow(dead_code)]
pub(crate) fn rewind(connection: &Connection, session_id: &str, boundary: i64) -> Result<(), BusError> {
    connection.execute(
        "DELETE FROM instruction_state WHERE session_id = ?1 AND through_seq >= ?2",
        rusqlite::params![session_id, boundary],
    )?;
    Ok(())
}

#[allow(dead_code)]
pub(crate) fn initialize_entries(
    connection: &Connection,
    session_id: &str,
    entries: &[(String, Value, bool)],
    created: i64,
) -> Result<(), BusError> {
    for batch in entries.chunks(ENTRY_INSERT_BATCH) {
        let placeholders = vec!["(?, ?, ?, ?, ?, ?)"; batch.len()].join(", ");
        let mut values = Vec::with_capacity(batch.len() * 6);
        for (key, value, removed) in batch {
            values.push(SqlValue::Text(session_id.to_string()));
            values.push(SqlValue::Text(key.clone()));
            values.push(SqlValue::Text(encode_json(value)));
            values.push(SqlValue::Integer(i64::from(*removed)));
            values.push(SqlValue::Integer(created));
            values.push(SqlValue::Integer(created));
        }
        connection.execute(
            &format!(
                "INSERT INTO instruction_entry (session_id, key, value, removed, time_created, time_updated) \
                 VALUES {placeholders}"
            ),
            params_from_iter(values),
        )?;
    }
    Ok(())
}

fn apply_hash_delta(mut values: Vec<(String, String)>, delta: &[(String, Option<String>)]) -> Vec<(String, String)> {
    for (key, value) in delta {
        match value {
            Some(value) => match values.iter_mut().find(|(existing, _)| existing == key) {
                Some((_, current)) => *current = value.clone(),
                None => values.push((key.clone(), value.clone())),
            },
            None => values.retain(|(existing, _)| existing != key),
        }
    }
    values
}

fn decode_values(text: &str) -> Result<Vec<(String, String)>, BusError> {
    let object: Map<String, Value> = serde_json::from_str(text).map_err(|error| invalid(error.to_string()))?;
    object
        .into_iter()
        .map(|(key, value)| match value {
            Value::String(value) => Ok((key, value)),
            _ => Err(invalid(format!("instruction hash for {key} is not a string"))),
        })
        .collect()
}

fn encode_values(values: &[(String, String)]) -> String {
    let object = values.iter().map(|(key, value)| (key.clone(), Value::String(value.clone()))).collect::<Map<_, _>>();
    encode_json(&Value::Object(object))
}

fn invalid(message: String) -> BusError {
    BusError::InvalidDurableEvent { r#type: "instruction_state".to_string(), message }
}
