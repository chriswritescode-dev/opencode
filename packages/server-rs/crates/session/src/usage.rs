use std::sync::Arc;

use opencode_bus::{Bus, BusError, Definition, Event, PublishOptions};
use opencode_storage::Database;
use rusqlite::OptionalExtension;
use tokio::task::JoinHandle;

pub fn spawn_usage_publisher(bus: Arc<Bus>, database: Arc<Database>) -> JoinHandle<()> {
    let mut subscription =
        bus.subscribe(&["session.step.ended", "session.step.failed", "session.usage.recorded"], None);
    tokio::spawn(async move {
        while let Some(event) = subscription.recv().await {
            if let Err(error) = publish_for_event(&bus, &database, &event) {
                eprintln!(
                    "Session usage publisher failed: eventID={} eventType={} error={error}",
                    event.id(),
                    event.r#type()
                );
                break;
            }
        }
    })
}

fn publish_for_event(bus: &Bus, database: &Database, event: &Event) -> Result<(), BusError> {
    let data = event.data();
    let session_id = match event.r#type() {
        "session.usage.recorded" | "session.step.ended" => data.get("sessionID").and_then(serde_json::Value::as_str),
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
    publish_session_usage(bus, database, session_id)
}

fn publish_session_usage(bus: &Bus, database: &Database, session_id: &str) -> Result<(), BusError> {
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
    bus.publish(
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
}
