use opencode_bus::{BusError, Event};
use serde_json::{Map, Value, json};

use crate::history;
use crate::parts::PartMatch;

pub(crate) trait Adapter {
    fn get_agent(&self) -> Result<Option<String>, BusError>;
    fn get_model(&self) -> Result<Option<Value>, BusError>;
    fn get_location(&self) -> Result<Option<Value>, BusError>;
    fn get_current_assistant(&self) -> Result<Option<Value>, BusError>;
    fn get_assistant(&self, message_id: &str) -> Result<Option<Value>, BusError>;
    fn get_shell(&self, shell_id: &str) -> Result<Option<Value>, BusError>;
    fn get_compaction(&self) -> Result<Option<Value>, BusError>;
    fn update_assistant(&self, assistant: Value) -> Result<(), BusError>;
    fn append_assistant_part(&self, message_id: &str, part: Value) -> Result<(), BusError>;
    fn update_assistant_part(
        &self,
        message_id: &str,
        part_match: PartMatch,
        recipe: &mut dyn FnMut(&mut Value),
    ) -> Result<(), BusError>;
    fn replace_assistant_parts(&self, message_id: &str, parts: Vec<Value>) -> Result<(), BusError>;
    fn update_shell(&self, shell: Value) -> Result<(), BusError>;
    fn update_compaction(&self, compaction: Value) -> Result<(), BusError>;
    fn append_message(&self, message: Value) -> Result<(), BusError>;
}

pub(crate) fn update(adapter: &dyn Adapter, event: &Event) -> Result<(), BusError> {
    let data = event.data();
    match event.r#type() {
        "session.created"
        | "session.viewed"
        | "session.archived"
        | "session.unarchived"
        | "session.usage.recorded"
        | "session.renamed"
        | "session.metadata.updated"
        | "session.permissions"
        | "session.deleted"
        | "session.forked"
        | "session.inbox.delivered"
        | "session.inbox.enqueued"
        | "session.inbox.cancelled"
        | "session.inbox.delivery.changed"
        | "session.execution.started"
        | "session.revert.staged"
        | "session.revert.cleared"
        | "session.revert.committed" => Ok(()),
        "session.agent.selected" => {
            let previous = match data.get("previous").filter(|value| !value.is_null()) {
                Some(previous) => Some(previous.clone()),
                None => adapter.get_agent()?.map(Value::String),
            };
            let mut fields = vec![("agent", Value::String(text_value(event, data, "agent")?))];
            push(&mut fields, "previous", previous);
            adapter.append_message(message(event, "agent-switched", envelope_metadata(event), fields))
        }
        "session.model.selected" => {
            let previous = match data.get("previous").filter(|value| !value.is_null()) {
                Some(previous) => Some(previous.clone()),
                None => adapter.get_model()?,
            };
            let mut fields = vec![("model", required_value(event, data, "model")?)];
            push(&mut fields, "previous", previous);
            adapter.append_message(message(event, "model-switched", envelope_metadata(event), fields))
        }
        "session.moved" => {
            let mut fields = vec![("location", required_value(event, data, "location")?)];
            push(&mut fields, "projectID", data.get("projectID").filter(|value| !value.is_null()).cloned());
            push(&mut fields, "subpath", data.get("subpath").filter(|value| !value.is_null()).cloned());
            push(&mut fields, "previous", adapter.get_location()?);
            adapter.append_message(message(event, "location-switched", envelope_metadata(event), fields))
        }
        "session.synthetic" => {
            let mut fields = vec![("text", Value::String(text_value(event, data, "text")?))];
            push(&mut fields, "description", data.get("description").filter(|value| !value.is_null()).cloned());
            let metadata = data.get("metadata").filter(|value| !value.is_null()).cloned();
            adapter.append_message(message(event, "synthetic", metadata, fields))
        }
        "session.skill.activated" => adapter.append_message(message(
            event,
            "skill",
            envelope_metadata(event),
            vec![
                ("skill", Value::String(text_value(event, data, "id")?)),
                ("name", Value::String(text_value(event, data, "name")?)),
                ("text", Value::String(text_value(event, data, "text")?)),
            ],
        )),
        "session.shell.started" => {
            let shell = required_object(event, data, "shell")?;
            adapter.append_message(message(
                event,
                "shell",
                shell_metadata(event),
                vec![
                    ("shellID", Value::String(text_field(event, &shell, "id")?)),
                    ("command", Value::String(text_field(event, &shell, "command")?)),
                    ("status", Value::String(text_field(event, &shell, "status")?)),
                ],
            ))
        }
        "session.shell.ended" => ended(adapter, event),
        "session.execution.succeeded" => idle(adapter, event, "succeeded", None),
        "session.execution.failed" => {
            idle(adapter, event, "failed", data.get("error").filter(|value| !value.is_null()).cloned())
        }
        "session.execution.interrupted" => {
            if data.get("reason").and_then(Value::as_str) == Some("shutdown") {
                return clear_current_retry(adapter);
            }
            idle(adapter, event, "interrupted", None)
        }
        "session.instructions.updated" => {
            let Some(text) = data.get("text").filter(|value| !value.is_null()).and_then(Value::as_str) else {
                return Ok(());
            };
            let delta = data
                .get("delta")
                .and_then(Value::as_object)
                .map(|delta| delta.keys().cloned().collect::<Vec<String>>())
                .unwrap_or_default();
            let mut metadata =
                envelope_metadata(event).and_then(|value| value.as_object().cloned()).unwrap_or_default();
            metadata.insert("notice".to_string(), Value::String("instructions".to_string()));
            metadata.insert(
                "instructionSources".to_string(),
                Value::Array(delta.iter().cloned().map(Value::String).collect()),
            );
            adapter.append_message(message(
                event,
                "system",
                Some(Value::Object(metadata)),
                vec![
                    ("text", Value::String(text.to_string())),
                    ("description", Value::String(format!("Instructions updated: {}", delta.join(", ")))),
                ],
            ))
        }
        "session.compaction.started" => adapter.append_message(message_at(
            data.get("inputID")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| history::message_id_from_event(event.id())),
            "compaction",
            envelope_metadata(event),
            Value::from(event.created()),
            vec![
                ("status", Value::String("running".to_string())),
                ("reason", required_value(event, data, "reason")?),
                ("summary", Value::String(String::new())),
            ],
        )),
        "session.compaction.ended" => {
            let text = text_value(event, data, "text")?;
            let reason = required_value(event, data, "reason")?;
            let model = present(data, "model").cloned();
            let native = present(data, "native").cloned();
            let cost = present(data, "cost").cloned();
            let tokens = present(data, "tokens").cloned();
            if let Some(current) = adapter.get_compaction()? {
                let object = current.as_object().ok_or_else(|| invalid(event, "expected a stored compaction"))?;
                let id = text_field(event, object, "id")?;
                let created = object
                    .get("time")
                    .and_then(|time| time.get("created"))
                    .cloned()
                    .unwrap_or_else(|| Value::from(event.created()));
                let metadata = match envelope_metadata(event) {
                    Some(event_metadata) => merge_metadata(object.get("metadata"), &event_metadata),
                    None => object.get("metadata").filter(|value| !value.is_null()).cloned(),
                };
                let mut fields = vec![("status", Value::String("completed".to_string())), ("reason", reason)];
                push(&mut fields, "model", model);
                push(&mut fields, "native", native);
                fields.push(("summary", Value::String(text)));
                push(&mut fields, "cost", cost);
                push(&mut fields, "tokens", tokens);
                return adapter.update_compaction(message_at(id, "compaction", metadata, created, fields));
            }
            let mut fields = vec![("status", Value::String("completed".to_string())), ("reason", reason)];
            push(&mut fields, "model", model);
            push(&mut fields, "native", native);
            fields.push(("summary", Value::String(text)));
            push(&mut fields, "cost", cost);
            push(&mut fields, "tokens", tokens);
            adapter.append_message(message(event, "compaction", envelope_metadata(event), fields))
        }
        "session.compaction.failed" => {
            let reason = required_value(event, data, "reason")?;
            let error = required_value(event, data, "error")?;
            let cost = present(data, "cost").cloned();
            let tokens = present(data, "tokens").cloned();
            let current = adapter.get_compaction()?.and_then(|current| current.as_object().cloned());
            let id = current
                .as_ref()
                .and_then(|object| object.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| data.get("inputID").and_then(Value::as_str).map(str::to_string))
                .unwrap_or_else(|| history::message_id_from_event(event.id()));
            let metadata = current
                .as_ref()
                .and_then(|object| object.get("metadata"))
                .filter(|value| !value.is_null())
                .cloned()
                .or_else(|| envelope_metadata(event));
            let created = current
                .as_ref()
                .and_then(|object| object.get("time"))
                .and_then(|time| time.get("created"))
                .cloned()
                .unwrap_or_else(|| Value::from(event.created()));
            let mut fields =
                vec![("status", Value::String("failed".to_string())), ("reason", reason), ("error", error)];
            push(&mut fields, "cost", cost);
            push(&mut fields, "tokens", tokens);
            let failed = message_at(id, "compaction", metadata, created, fields);
            match current {
                Some(_) => adapter.update_compaction(failed),
                None => adapter.append_message(failed),
            }
        }
        "session.step.started" => step_started(adapter, event),
        "session.step.first-token" => {
            let message_id = text_value(event, data, "assistantMessageID")?;
            let created = Value::from(event.created());
            update_owned_assistant(adapter, event, &message_id, |object| {
                with_time(object, |time| {
                    time.insert("firstToken".to_string(), created);
                });
            })
        }
        "session.step.streamed" => {
            let message_id = text_value(event, data, "assistantMessageID")?;
            let created = Value::from(event.created());
            update_owned_assistant(adapter, event, &message_id, |object| {
                with_time(object, |time| {
                    time.insert("streamed".to_string(), created);
                });
            })
        }
        "session.step.ended" => {
            let message_id = text_value(event, data, "assistantMessageID")?;
            let finish = Value::String(text_value(event, data, "finish")?);
            let raw_finish = present(data, "rawFinish").cloned();
            let native = present(data, "native").cloned();
            let cost = required_value(event, data, "cost")?;
            let tokens = required_value(event, data, "tokens")?;
            let created = Value::from(event.created());
            update_owned_assistant(adapter, event, &message_id, |object| {
                with_time(object, |time| {
                    time.insert("completed".to_string(), created);
                });
                object.insert("finish".to_string(), finish);
                set_optional(object, "rawFinish", raw_finish);
                set_optional(object, "native", native);
                object.insert("cost".to_string(), cost);
                object.insert("tokens".to_string(), tokens);
                project_terminal_snapshot(object, data);
            })
        }
        "session.step.failed" => {
            let message_id = text_value(event, data, "assistantMessageID")?;
            let finish = Value::String(data.get("finish").and_then(Value::as_str).unwrap_or("error").to_string());
            let raw_finish = present(data, "rawFinish").cloned();
            let native = present(data, "native").cloned();
            let error = required_value(event, data, "error")?;
            let usage = match (present(data, "cost"), present(data, "tokens")) {
                (Some(cost), Some(tokens)) => Some((cost.clone(), tokens.clone())),
                _ => None,
            };
            let created = Value::from(event.created());
            update_owned_assistant(adapter, event, &message_id, |object| {
                with_time(object, |time| {
                    time.insert("completed".to_string(), created);
                });
                object.insert("finish".to_string(), finish);
                set_optional(object, "rawFinish", raw_finish);
                set_optional(object, "native", native);
                object.insert("error".to_string(), error);
                object.shift_remove("retry");
                if let Some((cost, tokens)) = usage {
                    object.insert("cost".to_string(), cost);
                    object.insert("tokens".to_string(), tokens);
                }
                project_terminal_snapshot(object, data);
            })
        }
        "session.text.started" => {
            let message_id = text_value(event, data, "assistantMessageID")?;
            adapter.append_assistant_part(&message_id, json!({ "type": "text", "text": "" }))
        }
        "session.text.ended" => {
            let message_id = text_value(event, data, "assistantMessageID")?;
            let text = required_value(event, data, "text")?;
            let native = present(data, "native").cloned();
            adapter.update_assistant_part(
                &message_id,
                PartMatch { r#type: "text".to_string(), call_id: None, incomplete_reasoning: false },
                &mut |part| {
                    let Some(object) = part.as_object_mut() else {
                        return;
                    };
                    object.insert("text".to_string(), text.clone());
                    set_optional(object, "native", native.clone());
                },
            )
        }
        "session.tool.input.started" => {
            let message_id = text_value(event, data, "assistantMessageID")?;
            let id = text_value(event, data, "id")?;
            let name = text_value(event, data, "name")?;
            adapter.append_assistant_part(
                &message_id,
                json!({
                    "type": "tool",
                    "id": id,
                    "name": name,
                    "time": { "created": event.created() },
                    "state": { "status": "streaming", "input": "" },
                }),
            )
        }
        "session.tool.input.ended" => {
            let message_id = text_value(event, data, "assistantMessageID")?;
            let call_id = text_value(event, data, "id")?;
            let text = required_value(event, data, "text")?;
            adapter.update_assistant_part(
                &message_id,
                PartMatch { r#type: "tool".to_string(), call_id: Some(call_id), incomplete_reasoning: false },
                &mut |part| {
                    let Some(state) =
                        part.as_object_mut().and_then(|object| object.get_mut("state")).and_then(Value::as_object_mut)
                    else {
                        return;
                    };
                    if state.get("status").and_then(Value::as_str) == Some("streaming") {
                        state.insert("input".to_string(), text.clone());
                    }
                },
            )
        }
        "session.tool.called" => {
            let message_id = text_value(event, data, "assistantMessageID")?;
            let call_id = text_value(event, data, "id")?;
            let executed = present(data, "executed").cloned();
            let native = present(data, "native").cloned();
            let input = required_value(event, data, "input")?;
            let created = Value::from(event.created());
            adapter.update_assistant_part(
                &message_id,
                PartMatch { r#type: "tool".to_string(), call_id: Some(call_id), incomplete_reasoning: false },
                &mut |part| {
                    let Some(object) = part.as_object_mut() else {
                        return;
                    };
                    set_optional(object, "executed", executed.clone());
                    set_optional(object, "native", native.clone());
                    with_time(object, |time| {
                        time.insert("ran".to_string(), created.clone());
                    });
                    object.insert(
                        "state".to_string(),
                        json!({ "status": "running", "input": input.clone(), "metadata": {} }),
                    );
                },
            )
        }
        "session.tool.success" => {
            let message_id = text_value(event, data, "assistantMessageID")?;
            let call_id = text_value(event, data, "id")?;
            let executed = data.get("executed").and_then(Value::as_bool).unwrap_or(false);
            let native = present(data, "native").cloned();
            let content = required_value(event, data, "content")?;
            let metadata = present(data, "metadata").cloned();
            let created = Value::from(event.created());
            adapter.update_assistant_part(
                &message_id,
                PartMatch { r#type: "tool".to_string(), call_id: Some(call_id), incomplete_reasoning: false },
                &mut |part| {
                    let Some(object) = part.as_object_mut() else {
                        return;
                    };
                    let state = object.get("state").and_then(Value::as_object);
                    if state.and_then(|state| state.get("status")).and_then(Value::as_str) != Some("running") {
                        return;
                    }
                    let input = state.and_then(|state| state.get("input")).cloned().unwrap_or_else(|| json!({}));
                    let executed = executed || object.get("executed").and_then(Value::as_bool) == Some(true);
                    object.insert("executed".to_string(), Value::Bool(executed));
                    if let Some(native) = &native {
                        merge_native(object, native);
                    }
                    with_time(object, |time| {
                        time.insert("completed".to_string(), created.clone());
                    });
                    let mut completed = Map::new();
                    completed.insert("status".to_string(), json!("completed"));
                    completed.insert("input".to_string(), input);
                    completed.insert("content".to_string(), content.clone());
                    if let Some(metadata) = metadata.clone() {
                        completed.insert("metadata".to_string(), metadata);
                    }
                    object.insert("state".to_string(), Value::Object(completed));
                },
            )
        }
        "session.tool.failed" => {
            let message_id = text_value(event, data, "assistantMessageID")?;
            let call_id = text_value(event, data, "id")?;
            let executed = data.get("executed").and_then(Value::as_bool).unwrap_or(false);
            let native = present(data, "native").cloned();
            let error = required_value(event, data, "error")?;
            let content = present(data, "content").cloned();
            let metadata = present(data, "metadata").cloned();
            let created = Value::from(event.created());
            adapter.update_assistant_part(
                &message_id,
                PartMatch { r#type: "tool".to_string(), call_id: Some(call_id), incomplete_reasoning: false },
                &mut |part| {
                    let Some(object) = part.as_object_mut() else {
                        return;
                    };
                    let state = object.get("state").and_then(Value::as_object);
                    let status = state.and_then(|state| state.get("status")).and_then(Value::as_str);
                    if status != Some("streaming") && status != Some("running") {
                        return;
                    }
                    let input = match state.and_then(|state| state.get("input")).cloned() {
                        Some(Value::String(_)) | None => json!({}),
                        Some(input) => input,
                    };
                    let executed = executed || object.get("executed").and_then(Value::as_bool) == Some(true);
                    object.insert("executed".to_string(), Value::Bool(executed));
                    if let Some(native) = &native {
                        merge_native(object, native);
                    }
                    with_time(object, |time| {
                        time.insert("completed".to_string(), created.clone());
                    });
                    let mut failed = Map::new();
                    failed.insert("status".to_string(), json!("error"));
                    failed.insert("input".to_string(), input);
                    failed.insert("error".to_string(), error.clone());
                    if let Some(content) = content.clone() {
                        failed.insert("content".to_string(), content);
                    }
                    if let Some(metadata) = metadata.clone() {
                        failed.insert("metadata".to_string(), metadata);
                    }
                    object.insert("state".to_string(), Value::Object(failed));
                },
            )
        }
        "session.reasoning.started" => {
            let message_id = text_value(event, data, "assistantMessageID")?;
            let native = present(data, "native").cloned();
            let mut part = Map::new();
            part.insert("type".to_string(), json!("reasoning"));
            part.insert("text".to_string(), json!(""));
            if let Some(native) = native {
                part.insert("native".to_string(), native);
            }
            part.insert("time".to_string(), json!({ "created": event.created() }));
            adapter.append_assistant_part(&message_id, Value::Object(part))
        }
        "session.reasoning.ended" => {
            let message_id = text_value(event, data, "assistantMessageID")?;
            let text = required_value(event, data, "text")?;
            let native = present(data, "native").cloned();
            let created = Value::from(event.created());
            adapter.update_assistant_part(
                &message_id,
                PartMatch { r#type: "reasoning".to_string(), call_id: None, incomplete_reasoning: true },
                &mut |part| {
                    let Some(object) = part.as_object_mut() else {
                        return;
                    };
                    object.insert("text".to_string(), text.clone());
                    let started = object
                        .get("time")
                        .and_then(|time| time.get("created"))
                        .filter(|value| !value.is_null())
                        .cloned()
                        .unwrap_or_else(|| created.clone());
                    object.insert("time".to_string(), json!({ "created": started, "completed": created }));
                    if let Some(native) = &native {
                        object.insert("native".to_string(), native.clone());
                    }
                },
            )
        }
        "session.retry.scheduled" => {
            let message_id = text_value(event, data, "assistantMessageID")?;
            let attempt = numeric(event, data, "attempt")?;
            let at = numeric(event, data, "at")?;
            let error = required_value(event, data, "error")?;
            update_owned_assistant(adapter, event, &message_id, |object| {
                let mut retry = Map::new();
                retry.insert("attempt".to_string(), attempt);
                retry.insert("at".to_string(), at);
                retry.insert("error".to_string(), error);
                object.insert("retry".to_string(), Value::Object(retry));
            })
        }
        "session.message.content.updated" => {
            let message_id = text_value(event, data, "messageID")?;
            let content = data
                .get("content")
                .and_then(Value::as_array)
                .cloned()
                .ok_or_else(|| invalid(event, "missing content"))?;
            adapter.replace_assistant_parts(&message_id, content)
        }
        r#type => Err(BusError::InvalidDurableEvent {
            r#type: r#type.to_string(),
            message: format!("Unknown durable event type {}", r#type),
        }),
    }
}

fn step_started(adapter: &dyn Adapter, event: &Event) -> Result<(), BusError> {
    let data = event.data();
    let message_id = text_value(event, data, "assistantMessageID")?;
    let agent = text_value(event, data, "agent")?;
    let model = required_value(event, data, "model")?;
    let started = numeric(event, data, "started")?;
    if let Some(mut assistant) = adapter.get_assistant(&message_id)? {
        {
            let object = assistant.as_object_mut().ok_or_else(|| invalid(event, "expected a stored assistant"))?;
            object.insert("agent".to_string(), Value::String(agent));
            object.insert("model".to_string(), model);
            for key in ["retry", "error", "finish", "rawFinish", "native"] {
                object.shift_remove(key);
            }
            with_time(object, |time| {
                time.insert("created".to_string(), started);
                time.shift_remove("firstToken");
                time.shift_remove("streamed");
                time.shift_remove("completed");
            });
            if let Some(snapshot) = present(data, "snapshot") {
                set_snapshot_key(object, "start", snapshot.clone());
            }
        }
        return adapter.update_assistant(assistant);
    }
    if let Some(mut current) = adapter.get_current_assistant()? {
        {
            let object = current.as_object_mut().ok_or_else(|| invalid(event, "expected a stored assistant"))?;
            object.shift_remove("retry");
            with_time(object, |time| {
                time.insert("completed".to_string(), Value::from(event.created()));
            });
        }
        adapter.update_assistant(current)?;
    }
    let mut fields = vec![("agent", Value::String(agent)), ("model", model), ("content", Value::Array(Vec::new()))];
    if let Some(snapshot) = present(data, "snapshot") {
        fields.push(("snapshot", json!({ "start": snapshot })));
    }
    adapter.append_message(message_at(message_id, "assistant", envelope_metadata(event), started, fields))
}

fn update_owned_assistant(
    adapter: &dyn Adapter,
    event: &Event,
    message_id: &str,
    recipe: impl FnOnce(&mut Map<String, Value>),
) -> Result<(), BusError> {
    let Some(mut assistant) = adapter.get_assistant(message_id)? else {
        return Ok(());
    };
    let object = assistant.as_object_mut().ok_or_else(|| invalid(event, "expected a stored assistant"))?;
    recipe(object);
    adapter.update_assistant(assistant)
}

fn project_terminal_snapshot(object: &mut Map<String, Value>, data: &Value) {
    let snapshot = present(data, "snapshot").cloned();
    let files = present(data, "files").cloned();
    if snapshot.is_none() && files.is_none() {
        return;
    }
    if !matches!(object.get("snapshot"), Some(Value::Object(_))) {
        object.insert("snapshot".to_string(), Value::Object(Map::new()));
    }
    let Some(Value::Object(snapshot_object)) = object.get_mut("snapshot") else {
        return;
    };
    set_optional(snapshot_object, "end", snapshot);
    set_optional(snapshot_object, "files", files);
}

fn set_snapshot_key(object: &mut Map<String, Value>, key: &str, value: Value) {
    if !matches!(object.get("snapshot"), Some(Value::Object(_))) {
        object.insert("snapshot".to_string(), Value::Object(Map::new()));
    }
    if let Some(Value::Object(snapshot)) = object.get_mut("snapshot") {
        snapshot.insert(key.to_string(), value);
    }
}

fn merge_native(object: &mut Map<String, Value>, native: &Value) {
    match object.get_mut("native") {
        Some(Value::Object(existing)) => {
            if let Value::Object(native) = native {
                existing.extend(native.clone());
            }
        }
        _ => {
            object.insert("native".to_string(), native.clone());
        }
    }
}

fn with_time(object: &mut Map<String, Value>, recipe: impl FnOnce(&mut Map<String, Value>)) {
    if !matches!(object.get("time"), Some(Value::Object(_))) {
        object.insert("time".to_string(), Value::Object(Map::new()));
    }
    if let Some(Value::Object(time)) = object.get_mut("time") {
        recipe(time);
    }
}

fn set_optional(object: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    match value {
        Some(value) => {
            object.insert(key.to_string(), value);
        }
        None => {
            object.shift_remove(key);
        }
    }
}

fn present<'a>(data: &'a Value, key: &str) -> Option<&'a Value> {
    data.get(key).filter(|value| !value.is_null())
}

fn numeric(event: &Event, data: &Value, key: &str) -> Result<Value, BusError> {
    let value = required_value(event, data, key)?;
    match value.as_f64() {
        Some(value) => Ok(quantity(value)),
        None => Err(invalid(event, format!("expected a number for {key}"))),
    }
}

fn quantity(value: f64) -> Value {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 9.2e18 {
        return Value::from(value as i64);
    }
    serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number)
}

fn ended(adapter: &dyn Adapter, event: &Event) -> Result<(), BusError> {
    let data = event.data();
    let shell = required_object(event, data, "shell")?;
    let shell_id = text_field(event, &shell, "id")?;
    let Some(mut current) = adapter.get_shell(&shell_id)? else {
        return Ok(());
    };
    let status = text_field(event, &shell, "status")?;
    let Some(object) = current.as_object_mut() else {
        return Err(invalid(event, "expected a stored shell"));
    };
    object.insert("status".to_string(), Value::String(status));
    match shell.get("exit").filter(|value| !value.is_null()) {
        Some(exit) => {
            object.insert("exit".to_string(), exit.clone());
        }
        None => {
            object.shift_remove("exit");
        }
    }
    match data.get("output").filter(|value| !value.is_null()) {
        Some(output) => {
            object.insert("output".to_string(), output.clone());
        }
        None => {
            object.shift_remove("output");
        }
    }
    let completed = Value::from(event.created());
    match object.get_mut("time") {
        Some(Value::Object(time)) => {
            time.insert("completed".to_string(), completed);
        }
        _ => {
            object.insert("time".to_string(), json!({ "created": event.created(), "completed": completed }));
        }
    }
    adapter.update_shell(current)
}

fn idle(adapter: &dyn Adapter, event: &Event, outcome: &str, error: Option<Value>) -> Result<(), BusError> {
    clear_current_retry(adapter)?;
    let mut fields = vec![("outcome", Value::String(outcome.to_string()))];
    push(&mut fields, "error", error);
    adapter.append_message(message(event, "idle", envelope_metadata(event), fields))
}

fn clear_current_retry(adapter: &dyn Adapter) -> Result<(), BusError> {
    let Some(mut assistant) = adapter.get_current_assistant()? else {
        return Ok(());
    };
    let Some(object) = assistant.as_object_mut() else {
        return Ok(());
    };
    if object.get("retry").is_none_or(Value::is_null) {
        return Ok(());
    }
    object.shift_remove("retry");
    adapter.update_assistant(assistant)
}

fn message(event: &Event, r#type: &str, metadata: Option<Value>, fields: Vec<(&str, Value)>) -> Value {
    message_at(history::message_id_from_event(event.id()), r#type, metadata, Value::from(event.created()), fields)
}

fn message_at(id: String, r#type: &str, metadata: Option<Value>, created: Value, fields: Vec<(&str, Value)>) -> Value {
    let mut object = Map::new();
    object.insert("id".to_string(), Value::String(id));
    if let Some(metadata) = metadata {
        object.insert("metadata".to_string(), metadata);
    }
    object.insert("time".to_string(), json!({ "created": created }));
    object.insert("type".to_string(), Value::String(r#type.to_string()));
    for (key, value) in fields {
        object.insert(key.to_string(), value);
    }
    Value::Object(object)
}

fn push(fields: &mut Vec<(&'static str, Value)>, key: &'static str, value: Option<Value>) {
    if let Some(value) = value {
        fields.push((key, value));
    }
}

fn envelope_metadata(event: &Event) -> Option<Value> {
    event.payload().get("metadata").filter(|value| !value.is_null()).cloned()
}

fn merge_metadata(current: Option<&Value>, event: &Value) -> Option<Value> {
    let mut merged = current.and_then(Value::as_object).cloned().unwrap_or_default();
    if let Some(event) = event.as_object() {
        for (key, value) in event {
            merged.insert(key.clone(), value.clone());
        }
    }
    Some(Value::Object(merged))
}

fn shell_metadata(event: &Event) -> Option<Value> {
    let metadata = envelope_metadata(event);
    if event.data()["shell"]["metadata"]["background"].as_bool() != Some(true) {
        return metadata;
    }
    let Some(Value::Object(mut object)) = metadata else {
        return Some(json!({ "background": true }));
    };
    object.insert("background".to_string(), Value::Bool(true));
    Some(Value::Object(object))
}

fn required_value(event: &Event, data: &Value, key: &str) -> Result<Value, BusError> {
    data.get(key).filter(|value| !value.is_null()).cloned().ok_or_else(|| invalid(event, format!("missing {key}")))
}

fn required_object(event: &Event, data: &Value, key: &str) -> Result<Map<String, Value>, BusError> {
    required_value(event, data, key)?
        .as_object()
        .cloned()
        .ok_or_else(|| invalid(event, format!("expected an object for {key}")))
}

fn text_field(event: &Event, object: &Map<String, Value>, key: &str) -> Result<String, BusError> {
    match object.get(key).filter(|value| !value.is_null()).and_then(Value::as_str) {
        Some(value) => Ok(value.to_string()),
        None => Err(invalid(event, format!("missing {key}"))),
    }
}

fn text_value(event: &Event, data: &Value, key: &str) -> Result<String, BusError> {
    match required_value(event, data, key)?.as_str() {
        Some(value) => Ok(value.to_string()),
        None => Err(invalid(event, format!("expected a string for {key}"))),
    }
}

fn invalid(event: &Event, message: impl Into<String>) -> BusError {
    BusError::InvalidDurableEvent { r#type: event.r#type().to_string(), message: message.into() }
}
