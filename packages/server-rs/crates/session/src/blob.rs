use opencode_bus::BusError;
use rusqlite::Connection;

pub(crate) fn release_session(connection: &Connection, session_id: &str) -> Result<(), BusError> {
    connection.execute(
        "DELETE FROM blob WHERE hash IN (SELECT hash FROM blob_ref WHERE session_id = ?1) \
         AND NOT EXISTS (SELECT 1 FROM blob_ref other WHERE other.hash = blob.hash AND other.session_id <> ?1)",
        [session_id],
    )?;
    connection.execute("DELETE FROM blob_ref WHERE session_id = ?1", [session_id])?;
    Ok(())
}
