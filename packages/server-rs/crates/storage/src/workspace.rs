use opencode_contract::codec::encode_json;
use rusqlite::{Connection, Row, params};
use serde_json::Value;

use crate::StorageError;
use crate::id::now_ms;

const SELECT: &str = "SELECT id, provider, binding, created_at, last_used_at FROM workspace";

#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceRow {
    pub id: String,
    pub provider: String,
    pub binding: Option<Value>,
    pub created_at: i64,
    pub last_used_at: i64,
}

pub fn insert(
    db: &Connection,
    id: &str,
    provider: &str,
    binding: Option<&Value>,
) -> Result<WorkspaceRow, StorageError> {
    let now = now_ms();
    let binding = binding.cloned();
    db.prepare_cached(
        "INSERT INTO workspace (id, provider, binding, created_at, last_used_at) VALUES (?1, ?2, ?3, ?4, ?4)",
    )?
    .execute(params![id, provider, binding.as_ref().map(encode_json), now])?;
    Ok(WorkspaceRow { id: id.to_string(), provider: provider.to_string(), binding, created_at: now, last_used_at: now })
}

pub fn get(db: &Connection, id: &str) -> Result<Option<WorkspaceRow>, StorageError> {
    let mut statement = db.prepare_cached(&format!("{SELECT} WHERE id = ?1"))?;
    let mut rows = statement.query(params![id])?;
    Ok(collect(&mut rows)?.into_iter().next())
}

pub fn list(db: &Connection) -> Result<Vec<WorkspaceRow>, StorageError> {
    let mut statement = db.prepare_cached(&format!("{SELECT} ORDER BY created_at ASC, id ASC"))?;
    let mut rows = statement.query([])?;
    collect(&mut rows)
}

pub fn touch(db: &Connection, id: &str) -> Result<(), StorageError> {
    db.prepare_cached("UPDATE workspace SET last_used_at = ?1 WHERE id = ?2")?.execute(params![now_ms(), id])?;
    Ok(())
}

pub fn delete(db: &Connection, id: &str) -> Result<bool, StorageError> {
    Ok(db.prepare_cached("DELETE FROM workspace WHERE id = ?1")?.execute(params![id])? > 0)
}

fn collect(rows: &mut rusqlite::Rows<'_>) -> Result<Vec<WorkspaceRow>, StorageError> {
    let mut workspaces = Vec::new();
    while let Some(row) = rows.next()? {
        workspaces.push(read_workspace(row)?);
    }
    Ok(workspaces)
}

fn read_workspace(row: &Row<'_>) -> Result<WorkspaceRow, StorageError> {
    let binding = match row.get::<_, Option<String>>(2)? {
        Some(text) => Some(serde_json::from_str(&text)?),
        None => None,
    };
    Ok(WorkspaceRow {
        id: row.get(0)?,
        provider: row.get(1)?,
        binding,
        created_at: row.get(3)?,
        last_used_at: row.get(4)?,
    })
}
