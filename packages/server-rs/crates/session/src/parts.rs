use opencode_bus::BusError;
use opencode_contract::codec::encode_json;
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

use crate::blob::{self, Owner};
use crate::history;

pub(crate) struct PartMatch {
    pub r#type: String,
    pub call_id: Option<String>,
    pub incomplete_reasoning: bool,
}

const ROWS_PER_INSERT: usize = 12;

struct PartRow {
    message_id: String,
    session_id: String,
    ordinal: i64,
    r#type: String,
    call_id: Option<String>,
    time_created: i64,
    time_updated: i64,
    data: Value,
}

pub(crate) fn prepare_assistant_parts(
    connection: &Connection,
    session_id: &str,
    created: i64,
    message_id: &str,
    carry: bool,
) -> Result<Option<Owner>, BusError> {
    let row = connection
        .query_row(
            "SELECT id, seq, json_array_length(data, '$.content') FROM session_message \
             WHERE id = ?1 AND session_id = ?2 AND type = 'assistant'",
            rusqlite::params![message_id, session_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, Option<i64>>(2)?)),
        )
        .optional()?;
    let Some((_, seq, inline_count)) = row else {
        return Ok(None);
    };
    if inline_count.unwrap_or(0) > 0
        && let Some((time_created, data)) = connection
            .query_row("SELECT time_created, data FROM session_message WHERE id = ?1", [message_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .optional()?
    {
        let message = history::decode_message_row(message_id, "assistant", &data)?;
        if carry {
            let last = connection
                .query_row("SELECT max(ordinal) FROM session_part WHERE message_id = ?1", [message_id], |row| {
                    row.get::<_, Option<i64>>(0)
                })
                .optional()?
                .flatten();
            let first = last.unwrap_or(-1) + 1;
            let parts = message.get("content").and_then(Value::as_array).cloned().unwrap_or_default();
            let rows = parts
                .iter()
                .enumerate()
                .map(|(index, part)| {
                    Ok(PartRow {
                        message_id: message_id.to_string(),
                        session_id: session_id.to_string(),
                        ordinal: first + index as i64,
                        r#type: part_type(part)?.to_string(),
                        call_id: call_id(part),
                        time_created,
                        time_updated: created,
                        data: history::encode_part(part),
                    })
                })
                .collect::<Result<Vec<_>, BusError>>()?;
            insert_rows(connection, &rows)?;
        }
        connection.execute(
            "UPDATE session_message SET data = ?1 WHERE id = ?2 AND session_id = ?3",
            rusqlite::params![
                encode_json(&history::assistant_data(&message, Some(Value::Array(Vec::new())))?),
                message_id,
                session_id
            ],
        )?;
    }
    Ok(Some(Owner { session_id: session_id.to_string(), message_id: message_id.to_string(), seq }))
}

pub(crate) fn append(
    connection: &Connection,
    session_id: &str,
    created: i64,
    message_id: &str,
    part: &Value,
) -> Result<(), BusError> {
    let Some(owner) = prepare_assistant_parts(connection, session_id, created, message_id, true)? else {
        return Ok(());
    };
    let part = blob::externalize_part(connection, part)?;
    let data = history::encode_part(&part);
    let ordinal = connection.query_row(
        "SELECT coalesce(max(ordinal) + 1, 0) FROM session_part WHERE message_id = ?1",
        [message_id],
        |row| row.get::<_, i64>(0),
    )?;
    insert_rows(
        connection,
        &[PartRow {
            message_id: message_id.to_string(),
            session_id: session_id.to_string(),
            ordinal,
            r#type: part_type(&part)?.to_string(),
            call_id: call_id(&part),
            time_created: created,
            time_updated: created,
            data: data.clone(),
        }],
    )?;
    blob::record_refs(connection, &owner, &[data])
}

pub(crate) fn update(
    connection: &Connection,
    session_id: &str,
    created: i64,
    message_id: &str,
    part_match: &PartMatch,
    recipe: &mut dyn FnMut(&mut Value),
) -> Result<(), BusError> {
    let Some(owner) = prepare_assistant_parts(connection, session_id, created, message_id, true)? else {
        return Ok(());
    };
    let mut sql = String::from(
        "SELECT ordinal, type, data FROM session_part WHERE message_id = ?1 AND session_id = ?2 AND type = ?3",
    );
    let mut values = vec![
        SqlValue::Text(message_id.to_string()),
        SqlValue::Text(session_id.to_string()),
        SqlValue::Text(part_match.r#type.clone()),
    ];
    if let Some(call_id) = &part_match.call_id {
        values.push(SqlValue::Text(call_id.clone()));
        sql.push_str(&format!(" AND call_id = ?{}", values.len()));
    }
    if part_match.incomplete_reasoning {
        sql.push_str(" AND json_extract(data, '$.time.completed') IS NULL");
    }
    sql.push_str(" ORDER BY ordinal DESC LIMIT 1");
    let row = connection
        .query_row(&sql, rusqlite::params_from_iter(values), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        })
        .optional()?;
    let Some((ordinal, r#type, data)) = row else {
        return Ok(());
    };
    let mut current = history::decode_part_row(&r#type, &data)?;
    let before = history::encode_part(&current);
    recipe(&mut current);
    if history::encode_part(&current) == before {
        return Ok(());
    }
    let stored = blob::externalize_part(connection, &current)?;
    let data = history::encode_part(&stored);
    connection.execute(
        "UPDATE session_part SET data = ?1, time_updated = ?2 WHERE message_id = ?3 AND ordinal = ?4",
        rusqlite::params![encode_json(&data), created, message_id, ordinal],
    )?;
    if stored != current {
        blob::record_refs(connection, &owner, &[data])?;
    }
    Ok(())
}

pub(crate) fn replace(
    connection: &Connection,
    session_id: &str,
    created: i64,
    message_id: &str,
    parts: &[Value],
) -> Result<(), BusError> {
    let Some(owner) = prepare_assistant_parts(connection, session_id, created, message_id, false)? else {
        return Ok(());
    };
    connection.execute("DELETE FROM session_part WHERE message_id = ?1", [message_id])?;
    let rows = parts
        .iter()
        .enumerate()
        .map(|(ordinal, part)| {
            let stored = blob::externalize_part(connection, part)?;
            Ok(PartRow {
                message_id: message_id.to_string(),
                session_id: session_id.to_string(),
                ordinal: ordinal as i64,
                r#type: part_type(&stored)?.to_string(),
                call_id: call_id(&stored),
                time_created: created,
                time_updated: created,
                data: history::encode_part(&stored),
            })
        })
        .collect::<Result<Vec<_>, BusError>>()?;
    let content = rows.iter().map(|row| row.data.clone()).collect::<Vec<_>>();
    insert_rows(connection, &rows)?;
    blob::sync_refs(connection, &owner, &content)
}

fn part_type(part: &Value) -> Result<&str, BusError> {
    part.get("type").and_then(Value::as_str).ok_or_else(|| invalid("part", "expected a part type"))
}

fn call_id(part: &Value) -> Option<String> {
    if part.get("type").and_then(Value::as_str) != Some("tool") {
        return None;
    }
    part.get("id").and_then(Value::as_str).map(str::to_string)
}

fn insert_rows(connection: &Connection, rows: &[PartRow]) -> Result<(), BusError> {
    for batch in rows.chunks(ROWS_PER_INSERT) {
        let mut values = Vec::with_capacity(batch.len() * 8);
        let placeholders = batch
            .iter()
            .map(|row| {
                let first = values.len() + 1;
                values.push(SqlValue::Text(row.message_id.clone()));
                values.push(SqlValue::Text(row.session_id.clone()));
                values.push(SqlValue::Integer(row.ordinal));
                values.push(SqlValue::Text(row.r#type.clone()));
                values.push(row.call_id.clone().map_or(SqlValue::Null, SqlValue::Text));
                values.push(SqlValue::Integer(row.time_created));
                values.push(SqlValue::Integer(row.time_updated));
                values.push(SqlValue::Text(encode_json(&row.data)));
                format!(
                    "(?{first}, ?{}, ?{}, ?{}, ?{}, ?{}, ?{}, ?{})",
                    first + 1,
                    first + 2,
                    first + 3,
                    first + 4,
                    first + 5,
                    first + 6,
                    first + 7
                )
            })
            .collect::<Vec<_>>();
        connection.execute(
            &format!(
                "INSERT INTO session_part (message_id, session_id, ordinal, type, call_id, time_created, \
                 time_updated, data) VALUES {}",
                placeholders.join(", ")
            ),
            rusqlite::params_from_iter(values),
        )?;
    }
    Ok(())
}

fn invalid(r#type: &str, message: impl Into<String>) -> BusError {
    BusError::InvalidDurableEvent { r#type: r#type.to_string(), message: message.into() }
}
