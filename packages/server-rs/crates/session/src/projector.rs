use std::sync::Arc;

use opencode_bus::{Bus, BusError, Definition, Event, ListenerHandle, PublishOptions, versioned_type};
use opencode_contract::codec::{Decode, Encode, Path, encode_json};
use opencode_contract::generated::{
    DURABLE_EVENTS, SessionAgentSelectedData, SessionArchivedData, SessionCompactionEndedData, SessionCreatedData,
    SessionDeletedData, SessionExecutionFailedData, SessionExecutionInterruptedData,
    SessionExecutionInterruptedDataReason, SessionExecutionStartedData, SessionExecutionSucceededData,
    SessionInstructionsUpdatedData, SessionInstructionsUpdatedDataDeltaValue, SessionMetadataUpdatedData,
    SessionModelSelectedData, SessionMovedData, SessionPermissionsData, SessionRenamedData, SessionRevertClearedData,
    SessionRevertCommittedData, SessionRevertStagedData, SessionStepEndedData, SessionStepFailedData,
    SessionUnarchivedData, SessionUsageRecordedData, SessionViewedData, WorktreeResolvedData,
};
use opencode_storage::Database;
use opencode_util::path::Flavor;
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};

use crate::blob;
use crate::fork;
use crate::history;
use crate::inbox::{self, Delivered};
use crate::instruction;
use crate::message_updater::{self, Adapter};
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
    match event.r#type() {
        "session.created" => project_created(connection, event),
        "session.forked" => fork::project(connection, event, settings().fork_media),
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
        "session.inbox.enqueued" => {
            inbox::admitted(connection, settings().media_blob, event)?;
            touch(connection, event)
        }
        "session.inbox.delivered" => project_inbox_delivered(connection, event, settings),
        "session.inbox.cancelled" => inbox::cancelled(connection, event),
        "session.inbox.delivery.changed" => inbox::delivery_changed(connection, event),
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
        "session.instructions.updated" => {
            run(connection, event, settings)?;
            let data: SessionInstructionsUpdatedData = decode(event)?;
            let delta = data
                .delta
                .0
                .iter()
                .map(|(key, value)| match value {
                    SessionInstructionsUpdatedDataDeltaValue::String(hash) => (key.clone(), Some(hash.0.clone())),
                    SessionInstructionsUpdatedDataDeltaValue::String2(_) => (key.clone(), None),
                })
                .collect::<Vec<_>>();
            let seq = event
                .durable()
                .map_or_else(|| Err(invalid(event, "missing durable envelope")), |durable| Ok(durable.seq))?;
            instruction::apply(connection, &data.session_id.0, seq, &delta)
        }
        "session.compaction.started" | "session.compaction.failed" => run(connection, event, settings),
        "session.compaction.ended" => {
            run(connection, event, settings)?;
            let data: SessionCompactionEndedData = decode(event)?;
            let seq = event
                .durable()
                .map_or_else(|| Err(invalid(event, "missing durable envelope")), |durable| Ok(durable.seq))?;
            instruction::advance_epoch(connection, &data.session_id.0, seq)
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
        "session.revert.committed" => project_revert_committed(connection, event),
        "session.usage.recorded" => project_usage_recorded(connection, event),
        "session.deleted" => project_deleted(connection, event),
        "worktree.resolved" => project_worktree_resolved(connection, event),
        r#type => Err(BusError::InvalidDurableEvent {
            r#type: r#type.to_string(),
            message: format!("Unknown durable event type {}", r#type),
        }),
    }
}

fn run(connection: &Connection, event: &Event, settings: &dyn Fn() -> ProjectionSettings) -> Result<(), BusError> {
    let settings = settings();
    let adapter =
        ProjectorAdapter { connection, event, part_rows: settings.part_rows, media_blob: settings.media_blob };
    message_updater::update(&adapter, event)
}

struct ProjectorAdapter<'a> {
    connection: &'a Connection,
    event: &'a Event,
    part_rows: bool,
    media_blob: bool,
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
        if !self.part_rows {
            return self.update_message_inline(message);
        }
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

    fn stored_seq(&self, message_id: &str) -> Result<Option<i64>, BusError> {
        let seq = self
            .connection
            .query_row(
                "SELECT seq FROM session_message WHERE id = ?1 AND session_id = ?2",
                rusqlite::params![message_id, self.session_id()?],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        Ok(seq)
    }

    fn update_message_inline(&self, message: &serde_json::Value) -> Result<(), BusError> {
        let id = history::message_id(message)?;
        let session_id = self.session_id()?;
        let r#type = history::message_type(message)?;
        let stored = blob::externalize(self.connection, self.media_blob, message)?;
        let changed = stored != *message;
        let mut data = if r#type == "assistant" {
            history::assistant_data(&stored, None)?
        } else {
            history::stored_data(&stored)?
        };
        let content = if r#type == "assistant" {
            let content = data
                .get("content")
                .and_then(serde_json::Value::as_array)
                .map(|parts| parts.iter().map(history::encode_part).collect::<Vec<_>>())
                .unwrap_or_default();
            data.as_object_mut()
                .ok_or_else(|| invalid(self.event, "expected a message object"))?
                .insert("content".to_string(), serde_json::Value::Array(content.clone()));
            content
        } else {
            Vec::new()
        };
        let full = data.clone();
        let parts = if r#type == "assistant" { parts::content_rows(self.connection, &id)? } else { Vec::new() };
        if !parts.is_empty() {
            data.as_object_mut()
                .ok_or_else(|| invalid(self.event, "expected a message object"))?
                .insert("content".to_string(), serde_json::Value::Array(Vec::new()));
        }
        let updated = self.connection.execute(
            "UPDATE session_message SET type = ?1, time_created = ?2, data = ?3, time_updated = ?4 \
             WHERE id = ?5 AND session_id = ?6",
            rusqlite::params![
                r#type,
                number(history::message_created(message)?),
                encode_json(&data),
                now_millis(),
                id,
                session_id
            ],
        )?;
        if updated == 0 {
            return Ok(());
        }
        if !parts.is_empty() {
            parts::write_content(self.connection, session_id, self.event.created(), &id, &parts, &content)?;
        }
        let Some(seq) = self.stored_seq(&id)? else {
            return Ok(());
        };
        let owner = blob::Owner { session_id: session_id.to_string(), message_id: id, seq };
        if self.event.r#type() == "session.message.content.updated" {
            blob::sync_refs(self.connection, &owner, &[full])
        } else if changed {
            blob::record_refs(self.connection, &owner, &[full])
        } else {
            Ok(())
        }
    }

    fn with_parts(&self, mut message: serde_json::Value) -> Result<serde_json::Value, BusError> {
        let Some(id) = message.get("id").and_then(serde_json::Value::as_str).map(str::to_string) else {
            return Ok(message);
        };
        let rows = parts::content_rows(self.connection, &id)?;
        if rows.is_empty() {
            return Ok(message);
        }
        let mut content = rows
            .iter()
            .map(|row| history::decode_part_row(&row.r#type, &row.data))
            .collect::<Result<Vec<_>, BusError>>()?;
        let Some(object) = message.as_object_mut() else {
            return Ok(message);
        };
        if let Some(serde_json::Value::Array(inline)) = object.get("content") {
            content.extend(inline.iter().cloned());
        }
        object.insert("content".to_string(), serde_json::Value::Array(content));
        Ok(message)
    }

    fn edit_assistant(&self, message_id: &str, recipe: &mut dyn FnMut(&mut serde_json::Value)) -> Result<(), BusError> {
        let Some(mut assistant) = self.get_assistant(message_id)? else {
            return Ok(());
        };
        recipe(&mut assistant);
        self.update_message(&assistant)
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
        let message = if self.part_rows { message } else { self.with_parts(message)? };
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
        let Some((id, data)) = row else {
            return Ok(None);
        };
        let message = history::decode_message_row(&id, "assistant", &data)?;
        if self.part_rows {
            return Ok(Some(message));
        }
        Ok(Some(self.with_parts(message)?))
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
        if self.part_rows {
            return parts::append(
                self.connection,
                self.media_blob,
                self.session_id()?,
                self.event.created(),
                message_id,
                &part,
            );
        }
        self.edit_assistant(message_id, &mut |assistant| {
            let Some(object) = assistant.as_object_mut() else {
                return;
            };
            match object.get_mut("content") {
                Some(serde_json::Value::Array(content)) => content.push(part.clone()),
                _ => {
                    object.insert("content".to_string(), serde_json::Value::Array(vec![part.clone()]));
                }
            }
        })
    }

    fn update_assistant_part(
        &self,
        message_id: &str,
        part_match: parts::PartMatch,
        recipe: &mut dyn FnMut(&mut serde_json::Value),
    ) -> Result<(), BusError> {
        if self.part_rows {
            return parts::update(
                self.connection,
                self.media_blob,
                self.session_id()?,
                self.event.created(),
                message_id,
                &part_match,
                recipe,
            );
        }
        self.edit_assistant(message_id, &mut |assistant| {
            let Some(object) = assistant.as_object_mut() else {
                return;
            };
            let Some(serde_json::Value::Array(content)) = object.get_mut("content") else {
                return;
            };
            let found = content.iter_mut().rev().find(|item| {
                item.get("type").and_then(serde_json::Value::as_str) == Some(part_match.r#type.as_str())
                    && part_match.call_id.as_ref().is_none_or(|call_id| {
                        item.get("id").and_then(serde_json::Value::as_str) == Some(call_id.as_str())
                    })
                    && (!part_match.incomplete_reasoning
                        || item
                            .get("time")
                            .and_then(|time| time.get("completed"))
                            .is_none_or(serde_json::Value::is_null))
            });
            if let Some(found) = found {
                recipe(found);
            }
        })
    }

    fn replace_assistant_parts(&self, message_id: &str, parts: Vec<serde_json::Value>) -> Result<(), BusError> {
        if self.part_rows {
            return parts::replace(
                self.connection,
                self.media_blob,
                self.session_id()?,
                self.event.created(),
                message_id,
                &parts,
            );
        }
        self.edit_assistant(message_id, &mut |assistant| {
            if let Some(object) = assistant.as_object_mut() {
                object.insert("content".to_string(), serde_json::Value::Array(parts.clone()));
            }
        })
    }

    fn update_shell(&self, shell: serde_json::Value) -> Result<(), BusError> {
        self.update_message(&shell)
    }

    fn update_compaction(&self, compaction: serde_json::Value) -> Result<(), BusError> {
        self.update_message(&compaction)
    }

    fn append_message(&self, message: serde_json::Value) -> Result<(), BusError> {
        let seq = self
            .event
            .durable()
            .map_or_else(|| Err(invalid(self.event, "missing durable envelope")), |durable| Ok(durable.seq))?;
        let stored = blob::externalize(self.connection, self.media_blob, &message)?;
        let r#type = history::message_type(&stored)?;
        let data = if r#type == "assistant" {
            history::assistant_data(&stored, None)?
        } else {
            history::stored_data(&stored)?
        };
        let id = history::message_id(&stored)?;
        self.connection.execute(
            "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                id,
                self.session_id()?,
                r#type,
                seq,
                number(history::message_created(&message)?),
                now_millis(),
                encode_json(&data),
            ],
        )?;
        let owner = blob::Owner { session_id: self.session_id()?.to_string(), message_id: id, seq };
        blob::record_refs(self.connection, &owner, &[data])
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

fn project_revert_committed(connection: &Connection, event: &Event) -> Result<(), BusError> {
    let data: SessionRevertCommittedData = decode(event)?;
    let boundary = connection
        .query_row(
            "SELECT seq FROM session_message WHERE session_id = ?1 AND id = ?2",
            params![data.session_id.0, data.to.0],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let Some(boundary) = boundary else {
        return Err(invalid(event, format!("Revert boundary message not found: {}", data.to.0)));
    };
    connection.execute(
        "DELETE FROM session_message WHERE session_id = ?1 AND seq >= ?2",
        params![data.session_id.0, boundary],
    )?;
    connection.execute(
        "DELETE FROM session_inbox WHERE session_id = ?1 AND enqueued_seq >= ?2",
        params![data.session_id.0, boundary],
    )?;
    set_session(
        connection,
        &data.session_id.0,
        &[Assignment::Value("revert", Value::Null), Assignment::Value("time_updated", Value::Integer(event.created()))],
    )?;
    instruction::rewind(connection, &data.session_id.0, boundary)?;
    blob::release_from(connection, &data.session_id.0, boundary)
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

fn project_inbox_delivered(
    connection: &Connection,
    event: &Event,
    settings: &dyn Fn() -> ProjectionSettings,
) -> Result<(), BusError> {
    let settings = settings();
    let adapter =
        ProjectorAdapter { connection, event, part_rows: settings.part_rows, media_blob: settings.media_blob };
    match inbox::delivered(connection, event)? {
        Delivered::User { id, payload } => {
            adapter.append_message(stored_message(event, &id, "user", &payload, &["files", "agents", "skills"])?)
        }
        Delivered::Synthetic { id, payload } => {
            adapter.append_message(stored_message(event, &id, "synthetic", &payload, &["description"])?)
        }
        Delivered::Removed => Ok(()),
    }
}

fn stored_message(
    event: &Event,
    id: &str,
    r#type: &str,
    payload: &serde_json::Value,
    extra: &[&str],
) -> Result<serde_json::Value, BusError> {
    let mut message = serde_json::Map::new();
    message.insert("id".to_string(), serde_json::Value::String(id.to_string()));
    if let Some(metadata) = payload.get("metadata") {
        message.insert("metadata".to_string(), metadata.clone());
    }
    message.insert("time".to_string(), serde_json::json!({ "created": event.created() }));
    let text = payload.get("text").cloned().ok_or_else(|| invalid(event, "missing text"))?;
    message.insert("text".to_string(), text);
    for key in extra {
        if let Some(value) = payload.get(*key) {
            message.insert((*key).to_string(), value.clone());
        }
    }
    message.insert("type".to_string(), serde_json::Value::String(r#type.to_string()));
    Ok(serde_json::Value::Object(message))
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

fn project_worktree_resolved(connection: &Connection, event: &Event) -> Result<(), BusError> {
    let data: WorktreeResolvedData = decode(event)?;
    let adopted = data.adopted.unwrap_or_default().into_iter().map(|project| project.0).collect::<Vec<_>>();
    let mut candidates = Vec::new();
    for candidate in std::iter::once(data.previous.0.clone())
        .chain(std::iter::once("global".to_string()))
        .chain(adopted.iter().cloned())
    {
        if candidate != data.project_id.0 && !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    if candidates.is_empty() {
        return Ok(());
    }
    let mut values = candidates.iter().cloned().map(Value::Text).collect::<Vec<_>>();
    let mut sql = format!(
        "SELECT s.id, s.directory, s.project_id, p.worktree FROM session_v2 s \
         INNER JOIN project p ON s.project_id = p.id WHERE s.project_id IN ({}) AND s.workspace_id IS NULL AND (",
        (1..=candidates.len()).map(|index| format!("?{index}")).collect::<Vec<_>>().join(", ")
    );
    if !adopted.is_empty() {
        let first = values.len() + 1;
        let placeholders =
            (first..first + adopted.len()).map(|index| format!("?{index}")).collect::<Vec<_>>().join(", ");
        values.extend(adopted.iter().cloned().map(Value::Text));
        sql.push_str(&format!("s.project_id IN ({placeholders}) OR "));
    }
    let low = values.len() + 1;
    let high = low + 1;
    values.push(Value::Text(data.directory.0.clone()));
    values.push(Value::Text(format!("{}\u{ffff}", data.directory.0)));
    sql.push_str(&format!("(s.directory >= ?{low} AND s.directory <= ?{high}))"));
    let rows = {
        let mut statement = connection.prepare(&sql)?;
        statement
            .query_map(params_from_iter(values), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let updates = rows
        .into_iter()
        .filter_map(|(session_id, directory, project_id, worktree)| {
            let directory =
                if adopted.contains(&project_id) { worktree } else { Flavor::NATIVE.resolve(&[&directory]) };
            contains(&data.directory.0, &directory)
                .then(|| (session_id, Flavor::NATIVE.relative(&data.directory.0, &directory).replace('\\', "/")))
        })
        .collect::<Vec<_>>();
    for (session_id, path) in updates {
        set_session(
            connection,
            &session_id,
            &[
                Assignment::Value("project_id", Value::Text(data.project_id.0.clone())),
                Assignment::Value("path", Value::Text(path)),
                Assignment::Keep("time_updated"),
            ],
        )?;
    }
    Ok(())
}

fn contains(parent: &str, child: &str) -> bool {
    let relative = Flavor::NATIVE.relative(parent, child);
    relative.is_empty()
        || (!Flavor::NATIVE.is_absolute(&relative)
            && relative != ".."
            && !relative.starts_with(&format!("..{}", Flavor::NATIVE.sep())))
}
