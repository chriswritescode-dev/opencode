use rusqlite::{Connection, Row, params};

use crate::StorageError;
use crate::id::{ascending, now_ms};

const SELECT: &str = "SELECT id, project_id, action, resource, time_created, time_updated FROM permission";
const ADD: &str = "INSERT INTO permission (id, project_id, action, resource, time_created, time_updated) \
                   VALUES (?1, ?2, ?3, ?4, ?5, ?5) ON CONFLICT DO NOTHING";
const UPDATE: &str = "UPDATE permission SET action = ?2, resource = ?3, time_updated = ?4 WHERE id = ?1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionRow {
    pub id: String,
    pub project_id: String,
    pub action: String,
    pub resource: String,
    pub time_created: i64,
    pub time_updated: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateOutcome {
    Updated(PermissionRow),
    NotFound,
    Conflict,
}

pub fn list(db: &Connection, project_id: Option<&str>) -> Result<Vec<PermissionRow>, StorageError> {
    let mut statement = match project_id {
        Some(_) => db.prepare_cached(&format!("{SELECT} WHERE project_id = ?1 ORDER BY rowid ASC"))?,
        None => db.prepare_cached(&format!("{SELECT} ORDER BY rowid ASC"))?,
    };
    let mut rows = match project_id {
        Some(project_id) => statement.query(params![project_id])?,
        None => statement.query([])?,
    };
    collect(&mut rows)
}

pub fn add(db: &Connection, project_id: &str, action: &str, resources: &[String]) -> Result<(), StorageError> {
    if resources.is_empty() {
        return Ok(());
    }
    let now = now_ms();
    let mut statement = db.prepare_cached(ADD)?;
    let transaction = db.is_autocommit().then(|| db.unchecked_transaction()).transpose()?;
    for resource in resources {
        statement.execute(params![format!("psv_{}", ascending()), project_id, action, resource, now])?;
    }
    if let Some(transaction) = transaction {
        transaction.commit()?;
    }
    Ok(())
}

pub fn get(db: &Connection, id: &str) -> Result<Option<PermissionRow>, StorageError> {
    let mut statement = db.prepare_cached(&format!("{SELECT} WHERE id = ?1"))?;
    let mut rows = statement.query(params![id])?;
    Ok(collect(&mut rows)?.into_iter().next())
}

pub fn update(
    db: &Connection,
    id: &str,
    action: Option<&str>,
    resource: Option<&str>,
) -> Result<UpdateOutcome, StorageError> {
    let Some(current) = get(db, id)? else {
        return Ok(UpdateOutcome::NotFound);
    };
    let action = action.unwrap_or(&current.action);
    let resource = resource.unwrap_or(&current.resource);
    let now = now_ms();
    match db.prepare_cached(UPDATE)?.execute(params![id, action, resource, now]) {
        Ok(_) => Ok(UpdateOutcome::Updated(PermissionRow {
            action: action.to_string(),
            resource: resource.to_string(),
            time_updated: now,
            ..current
        })),
        Err(error) if is_unique_violation(&error) => Ok(UpdateOutcome::Conflict),
        Err(error) => Err(error.into()),
    }
}

pub fn remove(db: &Connection, id: &str) -> Result<(), StorageError> {
    db.prepare_cached("DELETE FROM permission WHERE id = ?1")?.execute(params![id])?;
    Ok(())
}

fn collect(rows: &mut rusqlite::Rows<'_>) -> Result<Vec<PermissionRow>, StorageError> {
    let mut permissions = Vec::new();
    while let Some(row) = rows.next()? {
        permissions.push(read_permission(row)?);
    }
    Ok(permissions)
}

fn read_permission(row: &Row<'_>) -> Result<PermissionRow, StorageError> {
    Ok(PermissionRow {
        id: row.get(0)?,
        project_id: row.get(1)?,
        action: row.get(2)?,
        resource: row.get(3)?,
        time_created: row.get(4)?,
        time_updated: row.get(5)?,
    })
}

fn is_unique_violation(error: &rusqlite::Error) -> bool {
    let rusqlite::Error::SqliteFailure(failure, message) = error else {
        return false;
    };
    failure.code == rusqlite::ErrorCode::ConstraintViolation
        && (failure.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
            || message.as_deref().is_some_and(|message| {
                message.contains("UNIQUE") || message.contains("permission_project_action_resource_idx")
            }))
}
