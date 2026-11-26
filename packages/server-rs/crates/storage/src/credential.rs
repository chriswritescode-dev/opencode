use rusqlite::{Connection, Row, params};

use crate::StorageError;
use crate::id::now_ms;

macro_rules! select_credential {
    ($rest:literal) => {
        concat!("SELECT id, integration_id, label, value, active, time_created, time_updated FROM credential", $rest)
    };
}

const LIST_ALL: &str =
    select_credential!(" WHERE integration_id IS NOT NULL ORDER BY active ASC, time_created ASC, id ASC");
const LIST: &str = select_credential!(" WHERE integration_id = ?1 ORDER BY active ASC, time_created ASC, id ASC");
const GET: &str = select_credential!(" WHERE id = ?1");

const INSERT: &str = "INSERT INTO credential (id, integration_id, label, value, active, time_created, time_updated)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)";

const CLEAR_ACTIVE: &str = "UPDATE credential SET active = 0, time_updated = ?1 WHERE integration_id = ?2";
const SET_ACTIVE: &str = "UPDATE credential SET active = 1, time_updated = ?1 WHERE id = ?2";
const PIN_ACTIVE: &str = "UPDATE credential SET active = 1, time_updated = ?1 WHERE id = ?2";
const UPDATE_LABEL: &str = "UPDATE credential SET label = ?1, time_updated = ?2 WHERE id = ?3";
const UPDATE_VALUE: &str = "UPDATE credential SET value = ?1, time_updated = ?2 WHERE id = ?3";
const DELETE: &str = "DELETE FROM credential WHERE id = ?1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialRow {
    pub id: String,
    pub integration_id: Option<String>,
    pub label: String,
    pub value: String,
    pub active: Option<bool>,
    pub time_created: i64,
    pub time_updated: i64,
}

pub struct NewCredential<'a> {
    pub id: &'a str,
    pub integration_id: &'a str,
    pub label: &'a str,
    pub value: &'a str,
    pub active: bool,
}

pub fn list_all(db: &Connection) -> Result<Vec<CredentialRow>, StorageError> {
    let mut statement = db.prepare_cached(LIST_ALL)?;
    let mut rows = statement.query([])?;
    collect(&mut rows)
}

pub fn list(db: &Connection, integration_id: &str) -> Result<Vec<CredentialRow>, StorageError> {
    let mut statement = db.prepare_cached(LIST)?;
    let mut rows = statement.query(params![integration_id])?;
    collect(&mut rows)
}

pub fn get(db: &Connection, id: &str) -> Result<Option<CredentialRow>, StorageError> {
    let mut statement = db.prepare_cached(GET)?;
    let mut rows = statement.query(params![id])?;
    Ok(collect(&mut rows)?.into_iter().next())
}

pub fn insert(db: &Connection, row: &NewCredential<'_>) -> Result<(), StorageError> {
    db.prepare_cached(INSERT)?.execute(params![
        row.id,
        row.integration_id,
        row.label,
        row.value,
        row.active,
        now_ms()
    ])?;
    Ok(())
}

pub fn set_active(db: &Connection, integration_id: &str, id: &str) -> Result<(), StorageError> {
    let now = now_ms();
    db.prepare_cached(CLEAR_ACTIVE)?.execute(params![now, integration_id])?;
    db.prepare_cached(SET_ACTIVE)?.execute(params![now, id])?;
    Ok(())
}

pub fn pin_active(db: &Connection, id: &str) -> Result<(), StorageError> {
    db.prepare_cached(PIN_ACTIVE)?.execute(params![now_ms(), id])?;
    Ok(())
}

pub fn update_label(db: &Connection, id: &str, label: &str) -> Result<bool, StorageError> {
    let changed = db.prepare_cached(UPDATE_LABEL)?.execute(params![label, now_ms(), id])?;
    Ok(changed > 0)
}

pub fn update_value(db: &Connection, id: &str, value: &str) -> Result<bool, StorageError> {
    let changed = db.prepare_cached(UPDATE_VALUE)?.execute(params![value, now_ms(), id])?;
    Ok(changed > 0)
}

pub fn delete(db: &Connection, id: &str) -> Result<(), StorageError> {
    db.prepare_cached(DELETE)?.execute(params![id])?;
    Ok(())
}

fn collect(rows: &mut rusqlite::Rows<'_>) -> Result<Vec<CredentialRow>, StorageError> {
    let mut credentials = Vec::new();
    while let Some(row) = rows.next()? {
        credentials.push(read(row)?);
    }
    Ok(credentials)
}

fn read(row: &Row<'_>) -> Result<CredentialRow, StorageError> {
    Ok(CredentialRow {
        id: row.get(0)?,
        integration_id: row.get(1)?,
        label: row.get(2)?,
        value: row.get(3)?,
        active: row.get(4)?,
        time_created: row.get(5)?,
        time_updated: row.get(6)?,
    })
}
