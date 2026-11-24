use std::collections::HashMap;
use std::sync::LazyLock;

use opencode_bus::{BusError, Event, reserve_sequence};
use opencode_contract::codec::encode_json;
use opencode_contract::generated::{SessionForkBoundary, SessionForkedData};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde_json::Value;

use crate::projector::decode;
use crate::{blob, history, instruction, parts, slug};

const FORK_BATCH_SIZE: i64 = 500;

struct MessageRow {
    id: String,
    r#type: String,
    seq: i64,
    time_created: i64,
    time_updated: i64,
    data: String,
}

struct PartSource {
    message_id: String,
    ordinal: i64,
    r#type: String,
    call_id: Option<String>,
    time_created: i64,
    time_updated: i64,
    data: String,
}

static FORK_TITLE: LazyLock<regex_lite::Regex> =
    LazyLock::new(|| regex_lite::Regex::new(r"^(.+) \(fork #([0-9]+)\)$").expect("the fork title pattern compiles"));

pub(crate) fn project(connection: &Connection, event: &Event, fork_media: bool) -> Result<(), BusError> {
    let data: SessionForkedData = decode(event)?;
    let parent = connection
        .query_row(
            "SELECT project_id, workspace_id, directory, path, title, agent, model, metadata, permission, version \
             FROM session_v2 WHERE id = ?1",
            [data.parent_id.0.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, String>(9)?,
                ))
            },
        )
        .optional()?;
    let Some((project_id, workspace_id, directory, path, title, agent, model, metadata, permission, version)) = parent
    else {
        return Err(BusError::InvalidDurableEvent {
            r#type: "session.forked".to_string(),
            message: format!("Fork parent session not found: {}", data.parent_id.0),
        });
    };

    let (is_before, boundary_message_id) = match &data.boundary {
        SessionForkBoundary::Before(boundary) => (true, boundary.message_id.0.as_str()),
        SessionForkBoundary::Through(boundary) => (false, boundary.message_id.0.as_str()),
    };
    let boundary_seq = connection
        .query_row(
            "SELECT seq FROM session_message WHERE session_id = ?1 AND id = ?2",
            params![data.parent_id.0, boundary_message_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let Some(boundary_seq) = boundary_seq else {
        return Err(BusError::InvalidDurableEvent {
            r#type: "session.forked".to_string(),
            message: format!("Fork boundary message not found: {boundary_message_id}"),
        });
    };
    let copied_seq = connection.query_row(
        if is_before {
            "SELECT max(seq) FROM session_message WHERE session_id = ?1 AND seq < ?2"
        } else {
            "SELECT max(seq) FROM session_message WHERE session_id = ?1 AND seq <= ?2"
        },
        params![data.parent_id.0, boundary_seq],
        |row| row.get::<_, Option<i64>>(0),
    )?;

    let inserted = connection.execute(
        "INSERT INTO session_v2 (id, parent_id, fork_session_id, fork_boundary, project_id, workspace_id, slug, \
         directory, path, title, agent, model, metadata, permission, version, cost, tokens_input, tokens_output, \
         tokens_reasoning, tokens_cache_read, tokens_cache_write, time_created, time_updated) \
         VALUES (?1, NULL, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, 0, 0, 0, 0, 0, 0, ?15, ?15) \
         ON CONFLICT(id) DO NOTHING",
        params![
            data.session_id.0,
            data.parent_id.0,
            encode_json(&data.boundary),
            project_id,
            workspace_id,
            slug::create(),
            directory,
            path,
            fork_title(title.as_deref()),
            agent,
            model,
            metadata,
            permission,
            version,
            event.created(),
        ],
    )?;
    if inserted == 0 {
        return Err(BusError::InvalidDurableEvent {
            r#type: "session.forked".to_string(),
            message: "Session already projected".to_string(),
        });
    }

    if let Some(entries) = &data.instruction_entries {
        let entries =
            entries.iter().map(|entry| (entry.key.0.clone(), entry.value.clone(), entry.removed)).collect::<Vec<_>>();
        instruction::initialize_entries(connection, &data.session_id.0, &entries, event.created())?;
    }

    if let Some(copied_seq) = copied_seq {
        let base_id = history::message_id_from_event(event.id());
        let mut cursor = -1;
        loop {
            let rows = {
                let mut statement = connection.prepare(
                    "SELECT id, type, seq, time_created, time_updated, data FROM session_message \
                     WHERE session_id = ?1 AND seq > ?2 AND seq <= ?3 \
                     AND (type != 'assistant' OR json_extract(data, '$.time.completed') IS NOT NULL) \
                     AND (type != 'shell' OR json_extract(data, '$.status') != 'running') \
                     AND (type != 'compaction' OR json_extract(data, '$.status') != 'running') \
                     ORDER BY seq ASC LIMIT ?4",
                )?;
                statement
                    .query_map(params![data.parent_id.0, cursor, copied_seq, FORK_BATCH_SIZE], |row| {
                        Ok(MessageRow {
                            id: row.get(0)?,
                            r#type: row.get(1)?,
                            seq: row.get(2)?,
                            time_created: row.get(3)?,
                            time_updated: row.get(4)?,
                            data: row.get(5)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?
            };
            if rows.is_empty() {
                break;
            }

            let mut copied = Vec::with_capacity(rows.len());
            let mut next_cursor = cursor;
            for row in rows {
                next_cursor = row.seq;
                let forked_id = format!("{base_id}_{}", row.seq);
                let mut value =
                    serde_json::from_str::<Value>(&row.data).map_err(|error| invalid(event, error.to_string()))?;
                if fork_media {
                    value = blob::externalize_row(connection, &value)?;
                }
                if row.r#type == "compaction" {
                    value = remap_keep_id(connection, &data.parent_id.0, value, &base_id)?;
                }
                connection.execute(
                    "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        forked_id,
                        data.session_id.0,
                        row.r#type,
                        row.seq,
                        row.time_created,
                        row.time_updated,
                        encode_json(&value)
                    ],
                )?;
                copied.push((row, forked_id, value));
            }

            for (row, forked_id, value) in &copied {
                let owner =
                    blob::Owner { session_id: data.session_id.0.clone(), message_id: forked_id.clone(), seq: row.seq };
                blob::record_refs(connection, &owner, std::slice::from_ref(value))?;
            }

            let assistant = copied.iter().filter(|(row, ..)| row.r#type == "assistant").collect::<Vec<_>>();
            if !assistant.is_empty() {
                let mut values = vec![SqlValue::Text(data.parent_id.0.clone())];
                let mut placeholders = Vec::with_capacity(assistant.len());
                for (row, ..) in &assistant {
                    values.push(SqlValue::Text(row.id.clone()));
                    placeholders.push(format!("?{}", values.len()));
                }
                let parts = {
                    let mut statement = connection.prepare(&format!(
                        "SELECT message_id, ordinal, type, call_id, time_created, time_updated, data \
                         FROM session_part WHERE session_id = ?1 AND message_id IN ({}) \
                         ORDER BY message_id ASC, ordinal ASC",
                        placeholders.join(", ")
                    ))?;
                    statement
                        .query_map(params_from_iter(values), |row| {
                            Ok(PartSource {
                                message_id: row.get(0)?,
                                ordinal: row.get(1)?,
                                r#type: row.get(2)?,
                                call_id: row.get(3)?,
                                time_created: row.get(4)?,
                                time_updated: row.get(5)?,
                                data: row.get(6)?,
                            })
                        })?
                        .collect::<Result<Vec<_>, _>>()?
                };
                let forked_ids = assistant
                    .iter()
                    .map(|(row, forked_id, _)| (row.id.as_str(), forked_id.as_str()))
                    .collect::<HashMap<_, _>>();
                let mut stored = Vec::with_capacity(parts.len());
                let mut content = HashMap::<String, Vec<Value>>::new();
                for part in parts {
                    let message_id = forked_ids
                        .get(part.message_id.as_str())
                        .map(|message_id| (*message_id).to_string())
                        .ok_or_else(|| invalid(event, "forked part carries no message"))?;
                    let mut value =
                        serde_json::from_str::<Value>(&part.data).map_err(|error| invalid(event, error.to_string()))?;
                    if fork_media {
                        value = blob::externalize_row(connection, &value)?;
                    }
                    content.entry(message_id.clone()).or_default().push(value.clone());
                    stored.push(parts::PartRow {
                        message_id,
                        session_id: data.session_id.0.clone(),
                        ordinal: part.ordinal,
                        r#type: part.r#type,
                        call_id: part.call_id,
                        time_created: part.time_created,
                        time_updated: part.time_updated,
                        data: value,
                    });
                }
                parts::insert_rows(connection, &stored)?;
                for (row, forked_id, _) in &assistant {
                    let owner = blob::Owner {
                        session_id: data.session_id.0.clone(),
                        message_id: (*forked_id).clone(),
                        seq: row.seq,
                    };
                    blob::record_refs(
                        connection,
                        &owner,
                        &[
                            serde_json::json!({ "content": content.get(forked_id.as_str()).cloned().unwrap_or_default() }),
                        ],
                    )?;
                }
            }

            cursor = next_cursor;
        }
        reserve_sequence(connection, &data.session_id.0, copied_seq)?;
    }

    if let Some(instructions) = &data.instructions {
        let values = instructions.0.0.iter().map(|(key, hash)| (key.clone(), hash.0.clone())).collect::<Vec<_>>();
        let seq = event
            .durable()
            .map_or_else(|| Err(invalid(event, "missing durable envelope")), |durable| Ok(durable.seq))?;
        instruction::initialize(connection, &data.session_id.0, seq, &values)?;
    }
    Ok(())
}

fn remap_keep_id(connection: &Connection, parent_id: &str, mut value: Value, base_id: &str) -> Result<Value, BusError> {
    let keep_id = value
        .get("metadata")
        .and_then(|metadata| metadata.get("keepID"))
        .and_then(Value::as_str)
        .filter(|keep_id| keep_id.starts_with("msg_"))
        .map(str::to_string);
    let Some(keep_id) = keep_id else {
        return Ok(value);
    };
    let kept = connection
        .query_row(
            "SELECT seq FROM session_message WHERE session_id = ?1 AND id = ?2",
            params![parent_id, keep_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let Some(kept) = kept else {
        return Ok(value);
    };
    if let Some(metadata) = value.get_mut("metadata").and_then(Value::as_object_mut) {
        metadata.insert("keepID".to_string(), Value::String(format!("{base_id}_{kept}")));
    }
    Ok(value)
}

fn fork_title(value: Option<&str>) -> Option<String> {
    let value = value?;
    let Some(captures) = FORK_TITLE.captures(value) else {
        return Some(format!("{value} (fork #1)"));
    };
    let group = captures.get(1).map_or("", |group| group.as_str());
    let fork = captures.get(2).and_then(|group| group.as_str().parse::<i64>().ok()).unwrap_or(0) + 1;
    Some(format!("{group} (fork #{fork})"))
}

fn invalid(event: &Event, message: impl Into<String>) -> BusError {
    BusError::InvalidDurableEvent { r#type: event.r#type().to_string(), message: message.into() }
}

#[cfg(test)]
mod tests {
    use super::fork_title;

    #[test]
    fn fork_title_numbers_the_copy() {
        assert_eq!(fork_title(None), None);
        assert_eq!(fork_title(Some("Parent")), Some("Parent (fork #1)".to_string()));
        assert_eq!(fork_title(Some("Parent (fork #2)")), Some("Parent (fork #3)".to_string()));
    }
}
