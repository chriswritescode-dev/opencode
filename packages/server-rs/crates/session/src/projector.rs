use std::sync::Arc;

use opencode_bus::{Bus, BusError, Definition, Event, ListenerHandle, PublishOptions, versioned_type};
use opencode_contract::codec::{Decode, Encode, Path, encode_json};
use opencode_contract::generated::{
    DURABLE_EVENTS, SessionAgentSelectedData, SessionArchivedData, SessionCreatedData, SessionDeletedData,
    SessionExecutionFailedData, SessionExecutionInterruptedData, SessionExecutionInterruptedDataReason,
    SessionExecutionStartedData, SessionExecutionSucceededData, SessionMetadataUpdatedData, SessionModelSelectedData,
    SessionMovedData, SessionPermissionsData, SessionRenamedData, SessionRevertClearedData, SessionRevertStagedData,
    SessionStepEndedData, SessionStepFailedData, SessionUnarchivedData, SessionUsageRecordedData, SessionViewedData,
};
use opencode_storage::Database;
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension};

use crate::blob;
use crate::history;
use crate::message_updater;
use crate::parts;
use crate::settings::ProjectionSettings;
use crate::sql::{Assignment, TokenTotals, apply_usage, now_millis, number, set_session};

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
            let data = event.data();
            let session_id = match event.r#type() {
                "session.usage.recorded" | "session.step.ended" => {
                    data.get("sessionID").and_then(serde_json::Value::as_str)
                }
                "session.step.failed" => {
                    if data.get("cost").is_none_or(serde_json::Value::is_null)
                        || data.get("tokens").is_none_or(serde_json::Value::is_null)
                    {
                        return Ok(());
                    }
                    data.get("sessionID").and_then(serde_json::Value::as_str)
                }
                _ => return Ok(()),
            };
            let Some(session_id) = session_id else {
                return Ok(());
            };
            let row = database.read(|connection| {
                Ok(connection
                    .query_row(
                        "SELECT cost, tokens_input, tokens_output, tokens_reasoning, tokens_cache_read, \
                         tokens_cache_write FROM session_v2 WHERE id = ?1",
                        [session_id],
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
                    "sessionID": session_id,
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
        "session.agent.selected" => {
            run(connection, event, settings)?;
            project_agent_selected(connection, event)
        }
        "session.model.selected" => {
            run(connection, event, settings)?;
            project_model_selected(connection, event)
        }
        "session.moved" => {
            run(connection, event, settings)?;
            project_moved(connection, event)
        }
        "session.synthetic" | "session.skill.activated" | "session.shell.started" | "session.shell.ended" => {
            run(connection, event, settings)
        }
        "session.step.started" => {
            run(connection, event, settings)?;
            touch(connection, event)
        }
        "session.step.first-token"
        | "session.step.streamed"
        | "session.text.started"
        | "session.text.ended"
        | "session.reasoning.started"
        | "session.reasoning.ended"
        | "session.tool.input.started"
        | "session.tool.input.ended"
        | "session.tool.called"
        | "session.tool.success"
        | "session.tool.failed"
        | "session.retry.scheduled"
        | "session.message.content.updated" => run(connection, event, settings),
        "session.step.ended" => {
            run(connection, event, settings)?;
            let data: SessionStepEndedData = decode(event)?;
            let tokens = TokenTotals {
                input: data.tokens.input,
                output: data.tokens.output,
                reasoning: data.tokens.reasoning,
                cache_read: data.tokens.cache.read,
                cache_write: data.tokens.cache.write,
            };
            apply_usage(connection, &data.session_id.0, data.cost.0, &tokens, Some(event.created()))
        }
        "session.step.failed" => {
            run(connection, event, settings)?;
            let data: SessionStepFailedData = decode(event)?;
            match (data.cost, data.tokens) {
                (Some(cost), Some(tokens)) => {
                    let tokens = TokenTotals {
                        input: tokens.input,
                        output: tokens.output,
                        reasoning: tokens.reasoning,
                        cache_read: tokens.cache.read,
                        cache_write: tokens.cache.write,
                    };
                    apply_usage(connection, &data.session_id.0, cost.0, &tokens, Some(event.created()))
                }
                _ => touch(connection, event),
            }
        }
        "session.execution.succeeded" => {
            run(connection, event, settings)?;
            let data: SessionExecutionSucceededData = decode(event)?;
            project_idle(connection, &data.session_id.0, event.created(), "succeeded")
        }
        "session.execution.failed" => {
            run(connection, event, settings)?;
            let data: SessionExecutionFailedData = decode(event)?;
            project_idle(connection, &data.session_id.0, event.created(), "failed")
        }
        "session.execution.interrupted" => {
            run(connection, event, settings)?;
            let data: SessionExecutionInterruptedData = decode(event)?;
            if data.reason == SessionExecutionInterruptedDataReason::Shutdown {
                return Ok(());
            }
            project_idle(connection, &data.session_id.0, event.created(), "interrupted")
        }
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

fn run(connection: &Connection, event: &Event, _settings: &dyn Fn() -> ProjectionSettings) -> Result<(), BusError> {
    message_updater::update(&ProjectorAdapter { connection, event }, event)
}

struct ProjectorAdapter<'a> {
    connection: &'a Connection,
    event: &'a Event,
}

impl ProjectorAdapter<'_> {
    fn session_id(&self) -> Result<&str, BusError> {
        self.event
            .data()
            .get("sessionID")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| invalid(self.event, "missing sessionID"))
    }

    fn update_message(&self, message: &serde_json::Value) -> Result<(), BusError> {
        let id = history::message_id(message)?;
        let session_id = self.session_id()?;
        if history::message_type(message)? != "assistant" {
            self.connection.execute(
                "UPDATE session_message SET data = ?1, time_updated = ?2 WHERE id = ?3 AND session_id = ?4",
                rusqlite::params![encode_json(&history::stored_data(message)?), now_millis(), id, session_id],
            )?;
            return Ok(());
        }
        let content = self
            .connection
            .query_row(
                "SELECT json_extract(data, '$.content') FROM session_message WHERE id = ?1 AND session_id = ?2",
                rusqlite::params![id, session_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();
        let content = match content {
            Some(content) => serde_json::from_str(&content).map_err(|error| invalid(self.event, error.to_string()))?,
            None => serde_json::Value::Array(Vec::new()),
        };
        let data = encode_json(&history::assistant_data(message, Some(content))?);
        if self.event.r#type() == "session.step.started" {
            let created = number(history::message_created(message)?);
            self.connection.execute(
                "UPDATE session_message SET data = ?1, time_created = ?2, time_updated = ?3 \
                 WHERE id = ?4 AND session_id = ?5",
                rusqlite::params![data, created, now_millis(), id, session_id],
            )?;
            return Ok(());
        }
        self.connection.execute(
            "UPDATE session_message SET data = ?1, time_updated = ?2 WHERE id = ?3 AND session_id = ?4",
            rusqlite::params![data, now_millis(), id, session_id],
        )?;
        Ok(())
    }
}

impl message_updater::Adapter for ProjectorAdapter<'_> {
    fn get_agent(&self) -> Result<Option<String>, BusError> {
        let agent = self
            .connection
            .query_row("SELECT agent FROM session_v2 WHERE id = ?1", [self.session_id()?], |row| {
                row.get::<_, Option<String>>(0)
            })
            .optional()?;
        Ok(agent.flatten())
    }

    fn get_model(&self) -> Result<Option<serde_json::Value>, BusError> {
        let model = self
            .connection
            .query_row("SELECT model FROM session_v2 WHERE id = ?1", [self.session_id()?], |row| {
                row.get::<_, Option<String>>(0)
            })
            .optional()?;
        let Some(model) = model.flatten() else {
            return Ok(None);
        };
        serde_json::from_str(&model).map(Some).map_err(|error| invalid(self.event, error.to_string()))
    }

    fn get_location(&self) -> Result<Option<serde_json::Value>, BusError> {
        let row = self
            .connection
            .query_row(
                "SELECT directory, workspace_id, project_id, path FROM session_v2 WHERE id = ?1",
                [self.session_id()?],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((directory, workspace_id, project_id, subpath)) = row else {
            return Ok(None);
        };
        let mut location = serde_json::Map::new();
        location.insert("directory".to_string(), serde_json::Value::String(directory));
        if let Some(workspace_id) = workspace_id {
            location.insert("workspaceID".to_string(), serde_json::Value::String(workspace_id));
        }
        let mut previous = serde_json::Map::new();
        previous.insert("location".to_string(), serde_json::Value::Object(location));
        if let Some(project_id) = project_id {
            previous.insert("projectID".to_string(), serde_json::Value::String(project_id));
        }
        if let Some(subpath) = subpath {
            previous.insert("subpath".to_string(), serde_json::Value::String(subpath));
        }
        Ok(Some(serde_json::Value::Object(previous)))
    }

    fn get_current_assistant(&self) -> Result<Option<serde_json::Value>, BusError> {
        let row = self
            .connection
            .query_row(
                "SELECT id, data FROM session_message WHERE session_id = ?1 AND type = 'assistant' \
                 ORDER BY seq DESC LIMIT 1",
                [self.session_id()?],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((id, data)) = row else {
            return Ok(None);
        };
        let message = history::decode_message_row(&id, "assistant", &data)?;
        if message.get("time").and_then(|time| time.get("completed")).is_some_and(|value| !value.is_null()) {
            return Ok(None);
        }
        Ok(Some(message))
    }

    fn get_assistant(&self, message_id: &str) -> Result<Option<serde_json::Value>, BusError> {
        let row = self
            .connection
            .query_row(
                "SELECT id, data FROM session_message WHERE id = ?1 AND session_id = ?2 AND type = 'assistant'",
                rusqlite::params![message_id, self.session_id()?],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        row.map(|(id, data)| history::decode_message_row(&id, "assistant", &data)).transpose()
    }

    fn get_shell(&self, shell_id: &str) -> Result<Option<serde_json::Value>, BusError> {
        let row = self
            .connection
            .query_row(
                "SELECT id, data FROM session_message WHERE session_id = ?1 AND type = 'shell' \
                 AND json_extract(data, '$.shellID') = ?2 ORDER BY seq DESC LIMIT 1",
                rusqlite::params![self.session_id()?, shell_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        row.map(|(id, data)| history::decode_message_row(&id, "shell", &data)).transpose()
    }

    fn get_compaction(&self) -> Result<Option<serde_json::Value>, BusError> {
        let row = self
            .connection
            .query_row(
                "SELECT id, data FROM session_message WHERE session_id = ?1 AND type = 'compaction' \
                 AND json_extract(data, '$.status') = 'running' ORDER BY seq DESC LIMIT 1",
                [self.session_id()?],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        row.map(|(id, data)| history::decode_message_row(&id, "compaction", &data)).transpose()
    }

    fn update_assistant(&self, assistant: serde_json::Value) -> Result<(), BusError> {
        self.update_message(&assistant)
    }

    fn append_assistant_part(&self, message_id: &str, part: serde_json::Value) -> Result<(), BusError> {
        parts::append(self.connection, self.session_id()?, self.event.created(), message_id, &part)
    }

    fn update_assistant_part(
        &self,
        message_id: &str,
        part_match: parts::PartMatch,
        recipe: &mut dyn FnMut(&mut serde_json::Value),
    ) -> Result<(), BusError> {
        parts::update(self.connection, self.session_id()?, self.event.created(), message_id, &part_match, recipe)
    }

    fn replace_assistant_parts(&self, message_id: &str, parts: Vec<serde_json::Value>) -> Result<(), BusError> {
        parts::replace(self.connection, self.session_id()?, self.event.created(), message_id, &parts)
    }

    fn update_shell(&self, shell: serde_json::Value) -> Result<(), BusError> {
        self.update_message(&shell)
    }

    fn append_message(&self, message: serde_json::Value) -> Result<(), BusError> {
        let seq = self
            .event
            .durable()
            .map_or_else(|| Err(invalid(self.event, "missing durable envelope")), |durable| Ok(durable.seq))?;
        let r#type = history::message_type(&message)?;
        let data = if r#type == "assistant" {
            history::assistant_data(&message, None)?
        } else {
            history::stored_data(&message)?
        };
        self.connection.execute(
            "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                history::message_id(&message)?,
                self.session_id()?,
                r#type,
                seq,
                number(history::message_created(&message)?),
                now_millis(),
                encode_json(&data),
            ],
        )?;
        Ok(())
    }
}

fn invalid(event: &Event, message: impl Into<String>) -> BusError {
    BusError::InvalidDurableEvent { r#type: event.r#type().to_string(), message: message.into() }
}

pub(crate) fn decode<T: Decode>(event: &Event) -> Result<T, BusError> {
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
    apply_usage(connection, &data.session_id.0, data.cost.0, &tokens, None)
}

fn touch(connection: &Connection, event: &Event) -> Result<(), BusError> {
    let session_id = event
        .data()
        .get("sessionID")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invalid(event, "missing sessionID"))?;
    set_session(connection, session_id, &[Assignment::Value("time_updated", Value::Integer(event.created()))])
}

fn project_deleted(connection: &Connection, event: &Event) -> Result<(), BusError> {
    let data: SessionDeletedData = decode(event)?;
    blob::release_session(connection, &data.session_id.0)?;
    connection.execute("DELETE FROM session_v2 WHERE id = ?1", [data.session_id.0.as_str()])?;
    Ok(())
}

fn project_agent_selected(connection: &Connection, event: &Event) -> Result<(), BusError> {
    let data: SessionAgentSelectedData = decode(event)?;
    set_session(
        connection,
        &data.session_id.0,
        &[
            Assignment::Value("agent", Value::Text(data.agent.0)),
            Assignment::Value("time_updated", Value::Integer(event.created())),
        ],
    )
}

fn project_model_selected(connection: &Connection, event: &Event) -> Result<(), BusError> {
    let data: SessionModelSelectedData = decode(event)?;
    set_session(
        connection,
        &data.session_id.0,
        &[
            Assignment::Value("model", Value::Text(encode_json(&data.model))),
            Assignment::Value("time_updated", Value::Integer(event.created())),
        ],
    )
}

fn project_moved(connection: &Connection, event: &Event) -> Result<(), BusError> {
    let data: SessionMovedData = decode(event)?;
    let path = match data.subpath {
        Some(subpath) => Assignment::Value("path", Value::Text(subpath.0)),
        None => Assignment::Keep("path"),
    };
    set_session(
        connection,
        &data.session_id.0,
        &[
            Assignment::Value("directory", Value::Text(data.location.directory.0)),
            path,
            Assignment::Value("project_id", Value::Text(data.project_id.0)),
            Assignment::Value("workspace_id", optional_text(data.location.workspace_id.map(|workspace| workspace.0))),
            Assignment::Value("time_updated", Value::Integer(event.created())),
        ],
    )
}

fn project_idle(connection: &Connection, session_id: &str, created: i64, outcome: &str) -> Result<(), BusError> {
    connection.execute(
        "UPDATE session_v2 SET time_idle = max(?1, coalesce(time_idle + 1, ?1)), idle_outcome = ?2, \
         time_updated = time_updated WHERE id = ?3",
        rusqlite::params![created, outcome, session_id],
    )?;
    Ok(())
}
