use std::sync::Arc;

use opencode_bus::{Bus, BusError, Definition, Event, ListenerHandle, PublishOptions, versioned_type};
use opencode_contract::codec::{Decode, Encode, Path, encode_json};
use opencode_contract::generated::{
    DURABLE_EVENTS, SessionArchivedData, SessionCreatedData, SessionDeletedData, SessionExecutionStartedData,
    SessionMetadataUpdatedData, SessionPermissionsData, SessionRenamedData, SessionRevertClearedData,
    SessionRevertStagedData, SessionUnarchivedData, SessionUsageRecordedData, SessionViewedData,
};
use opencode_storage::Database;
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension};

use crate::blob;
use crate::settings::ProjectionSettings;
use crate::sql::{Assignment, TokenTotals, apply_usage, number, set_session};

pub struct SessionProjector;

impl SessionProjector {
    pub fn register(
        bus: &Arc<Bus>,
        database: &Arc<Database>,
        settings: Arc<dyn Fn() -> ProjectionSettings + Send + Sync>,
    ) -> ListenerHandle {
        for (r#type, _, _) in DURABLE_EVENTS {
            let definition = Definition::latest_durable(r#type).expect("a durable event type resolves");
            let version = definition.durable.expect("a durable definition carries its spec").version;
            let settings = settings.clone();
            bus.project(
                &versioned_type(r#type, version),
                Box::new(move |connection, event| project(settings.as_ref(), connection, event)),
            );
        }
        let published = bus.clone();
        let database = database.clone();
        bus.listen(Box::new(move |event: &Event| {
            if event.r#type() != "session.usage.recorded" {
                return Ok(());
            }
            let data: SessionUsageRecordedData = decode(event)?;
            let row = database.read(|connection| {
                Ok(connection
                    .query_row(
                        "SELECT cost, tokens_input, tokens_output, tokens_reasoning, tokens_cache_read, \
                         tokens_cache_write FROM session_v2 WHERE id = ?1",
                        [data.session_id.0.as_str()],
                        |row| {
                            Ok((
                                row.get::<_, f64>(0)?,
                                row.get::<_, f64>(1)?,
                                row.get::<_, f64>(2)?,
                                row.get::<_, f64>(3)?,
                                row.get::<_, f64>(4)?,
                                row.get::<_, f64>(5)?,
                            ))
                        },
                    )
                    .optional()?)
            })?;
            let Some((cost, input, output, reasoning, cache_read, cache_write)) = row else {
                return Ok(());
            };
            published.publish(
                &Definition::ephemeral("session.usage.updated"),
                serde_json::json!({
                    "sessionID": data.session_id.0,
                    "cost": cost,
                    "tokens": {
                        "input": input,
                        "output": output,
                        "reasoning": reasoning,
                        "cache": { "read": cache_read, "write": cache_write },
                    },
                }),
                PublishOptions::default(),
            )?;
            Ok(())
        }))
    }
}

fn project(settings: &dyn Fn() -> ProjectionSettings, connection: &Connection, event: &Event) -> Result<(), BusError> {
    settings();
    match event.r#type() {
        "session.created" => project_created(connection, event),
        "session.renamed" => project_renamed(connection, event),
        "session.metadata.updated" => project_metadata_updated(connection, event),
        "session.permissions" => project_permissions(connection, event),
        "session.viewed" => project_viewed(connection, event),
        "session.archived" => project_archived(connection, event),
        "session.unarchived" => project_unarchived(connection, event),
        "session.execution.started" => project_execution_started(connection, event),
        "session.revert.staged" => project_revert_staged(connection, event),
        "session.revert.cleared" => project_revert_cleared(connection, event),
        "session.usage.recorded" => project_usage_recorded(connection, event),
        "session.deleted" => project_deleted(connection, event),
        r#type => {
            Err(BusError::InvalidDurableEvent { r#type: r#type.to_string(), message: "not ported yet".to_string() })
        }
    }
}

fn decode<T: Decode>(event: &Event) -> Result<T, BusError> {
    T::decode(event.data(), &mut Path::new()).map_err(|error| BusError::InvalidDurableEvent {
        r#type: event.r#type().to_string(),
        message: error.to_string(),
    })
}

fn optional_text(value: Option<String>) -> Value {
    value.map_or(Value::Null, Value::Text)
}

fn optional_json<T: Encode>(value: Option<&T>) -> Value {
    value.map_or(Value::Null, |value| Value::Text(encode_json(value)))
}

fn project_created(connection: &Connection, event: &Event) -> Result<(), BusError> {
    let data: SessionCreatedData = decode(event)?;
    let created = event.created();
    let inserted = connection.execute(
        "INSERT INTO session_v2 (id, project_id, workspace_id, parent_id, slug, directory, path, title, agent, \
         model, metadata, permission, version, time_created, time_updated) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15) ON CONFLICT(id) DO NOTHING",
        rusqlite::params![
            data.session_id.0,
            data.project_id.0,
            optional_text(data.location.workspace_id.map(|workspace| workspace.0)),
            optional_text(data.parent_id.map(|parent| parent.0)),
            data.slug,
            data.location.directory.0,
            optional_text(data.subpath.map(|subpath| subpath.0)),
            data.title,
            optional_text(data.agent.map(|agent| agent.0)),
            optional_json(data.model.as_ref()),
            optional_json(data.metadata.as_ref()),
            optional_json(data.permissions.as_ref()),
            data.version,
            created,
            created,
        ],
    )?;
    if inserted == 0 {
        return Err(BusError::InvalidDurableEvent {
            r#type: "session.created".into(),
            message: "Session already projected".into(),
        });
    }
    Ok(())
}

fn project_renamed(connection: &Connection, event: &Event) -> Result<(), BusError> {
    let data: SessionRenamedData = decode(event)?;
    set_session(
        connection,
        &data.session_id.0,
        &[
            Assignment::Value("title", Value::Text(data.title)),
            Assignment::Value("time_updated", Value::Integer(event.created())),
        ],
    )
}

fn project_metadata_updated(connection: &Connection, event: &Event) -> Result<(), BusError> {
    let data: SessionMetadataUpdatedData = decode(event)?;
    set_session(
        connection,
        &data.session_id.0,
        &[
            Assignment::Value("metadata", Value::Text(encode_json(&data.metadata))),
            Assignment::Value("time_updated", Value::Integer(event.created())),
        ],
    )
}

fn project_permissions(connection: &Connection, event: &Event) -> Result<(), BusError> {
    let data: SessionPermissionsData = decode(event)?;
    set_session(
        connection,
        &data.session_id.0,
        &[
            Assignment::Value("permission", Value::Text(encode_json(&data.permissions))),
            Assignment::Value("time_updated", Value::Integer(event.created())),
        ],
    )
}

fn project_viewed(connection: &Connection, event: &Event) -> Result<(), BusError> {
    let data: SessionViewedData = decode(event)?;
    let idle = number(data.idle);
    connection.execute(
        "UPDATE session_v2 SET time_viewed = max(?1, coalesce(time_viewed, ?2)), \
         time_updated = time_updated WHERE id = ?3",
        rusqlite::params_from_iter([idle.clone(), idle, Value::Text(data.session_id.0)]),
    )?;
    Ok(())
}

fn project_archived(connection: &Connection, event: &Event) -> Result<(), BusError> {
    let data: SessionArchivedData = decode(event)?;
    set_session(
        connection,
        &data.session_id.0,
        &[
            Assignment::Value("time_archived", Value::Integer(event.created())),
            Assignment::Value("time_updated", Value::Integer(event.created())),
        ],
    )
}

fn project_unarchived(connection: &Connection, event: &Event) -> Result<(), BusError> {
    let data: SessionUnarchivedData = decode(event)?;
    set_session(
        connection,
        &data.session_id.0,
        &[
            Assignment::Value("time_archived", Value::Null),
            Assignment::Value("time_updated", Value::Integer(event.created())),
        ],
    )
}

fn project_execution_started(connection: &Connection, event: &Event) -> Result<(), BusError> {
    let data: SessionExecutionStartedData = decode(event)?;
    set_session(
        connection,
        &data.session_id.0,
        &[Assignment::Value("time_busy", Value::Integer(event.created())), Assignment::Keep("time_updated")],
    )
}

fn project_revert_staged(connection: &Connection, event: &Event) -> Result<(), BusError> {
    let data: SessionRevertStagedData = decode(event)?;
    set_session(
        connection,
        &data.session_id.0,
        &[
            Assignment::Value("revert", Value::Text(encode_json(&data.revert))),
            Assignment::Value("time_updated", Value::Integer(event.created())),
        ],
    )
}

fn project_revert_cleared(connection: &Connection, event: &Event) -> Result<(), BusError> {
    let data: SessionRevertClearedData = decode(event)?;
    set_session(
        connection,
        &data.session_id.0,
        &[Assignment::Value("revert", Value::Null), Assignment::Value("time_updated", Value::Integer(event.created()))],
    )
}

fn project_usage_recorded(connection: &Connection, event: &Event) -> Result<(), BusError> {
    let data: SessionUsageRecordedData = decode(event)?;
    let tokens = TokenTotals {
        input: data.tokens.input,
        output: data.tokens.output,
        reasoning: data.tokens.reasoning,
        cache_read: data.tokens.cache.read,
        cache_write: data.tokens.cache.write,
    };
    apply_usage(connection, &data.session_id.0, data.cost.0, &tokens)
}

fn project_deleted(connection: &Connection, event: &Event) -> Result<(), BusError> {
    let data: SessionDeletedData = decode(event)?;
    blob::release_session(connection, &data.session_id.0)?;
    connection.execute("DELETE FROM session_v2 WHERE id = ?1", [data.session_id.0.as_str()])?;
    Ok(())
}
