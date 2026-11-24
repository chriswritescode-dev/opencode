use opencode_bus::{BusError, Event};
use serde_json::{Map, Value, json};

use crate::history;

pub(crate) trait Adapter {
    fn get_agent(&self) -> Result<Option<String>, BusError>;
    fn get_model(&self) -> Result<Option<Value>, BusError>;
    fn get_location(&self) -> Result<Option<Value>, BusError>;
    fn get_current_assistant(&self) -> Result<Option<Value>, BusError>;
    fn get_shell(&self, shell_id: &str) -> Result<Option<Value>, BusError>;
    #[allow(dead_code)]
    fn get_compaction(&self) -> Result<Option<Value>, BusError>;
    fn update_assistant(&self, assistant: Value) -> Result<(), BusError>;
    fn update_shell(&self, shell: Value) -> Result<(), BusError>;
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
        r#type => {
            Err(BusError::InvalidDurableEvent { r#type: r#type.to_string(), message: "not ported yet".to_string() })
        }
    }
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
            object.remove("exit");
        }
    }
    match data.get("output").filter(|value| !value.is_null()) {
        Some(output) => {
            object.insert("output".to_string(), output.clone());
        }
        None => {
            object.remove("output");
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
    object.remove("retry");
    adapter.update_assistant(assistant)
}

fn message(event: &Event, r#type: &str, metadata: Option<Value>, fields: Vec<(&str, Value)>) -> Value {
    let mut object = Map::new();
    object.insert("id".to_string(), Value::String(history::message_id_from_event(event.id())));
    if let Some(metadata) = metadata {
        object.insert("metadata".to_string(), metadata);
    }
    object.insert("time".to_string(), json!({ "created": event.created() }));
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
