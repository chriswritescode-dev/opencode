use opencode_bus::BusError;
use rusqlite::Connection;
use serde_json::Value;

#[allow(dead_code)]
pub(crate) struct Owner {
    pub session_id: String,
    pub message_id: String,
    pub seq: i64,
}

pub(crate) fn release_session(connection: &Connection, session_id: &str) -> Result<(), BusError> {
    connection.execute(
        "DELETE FROM blob WHERE hash IN (SELECT hash FROM blob_ref WHERE session_id = ?1) \
         AND NOT EXISTS (SELECT 1 FROM blob_ref other WHERE other.hash = blob.hash AND other.session_id <> ?1)",
        [session_id],
    )?;
    connection.execute("DELETE FROM blob_ref WHERE session_id = ?1", [session_id])?;
    Ok(())
}

pub(crate) fn externalize_part(_connection: &Connection, part: &Value) -> Result<Value, BusError> {
    Ok(part.clone())
}

pub(crate) fn record_refs(_connection: &Connection, _owner: &Owner, _content: &[Value]) -> Result<(), BusError> {
    Ok(())
}

pub(crate) fn sync_refs(_connection: &Connection, _owner: &Owner, _content: &[Value]) -> Result<(), BusError> {
    Ok(())
}
