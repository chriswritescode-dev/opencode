//! The `kv` table: JSON values keyed by string, and range scans over the keys.

use opencode_contract::codec::encode_json;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

use crate::StorageError;
use crate::id::now_ms;

/// One stored key and the JSON value it holds.
pub struct Entry {
    /// The key the value is stored under.
    pub key: String,
    /// The decoded JSON value.
    pub value: Value,
}

/// The bounds of a key scan.
pub struct ScanOptions {
    /// Only keys from this prefix up to its successor are considered.
    pub prefix: String,
    /// Excludes keys up to and including this one.
    pub after: Option<String>,
    /// The maximum number of entries to return; clamped to 1 through 1000.
    pub limit: Option<f64>,
}

/// The entries a scan returned, and the key to continue after when more rows remain.
pub struct ScanResult {
    /// The entries in ascending key order.
    pub entries: Vec<Entry>,
    /// The last returned key when the scan was truncated.
    pub next: Option<String>,
}

/// Reads the value stored under `key`, or `None` when no row exists.
pub fn get(db: &Connection, key: &str) -> Result<Option<Value>, StorageError> {
    let value = db.query_row("SELECT value FROM kv WHERE key = ?", [key], |row| row.get::<_, String>(0)).optional()?;
    match value {
        Some(text) => Ok(Some(serde_json::from_str(&text)?)),
        None => Ok(None),
    }
}

/// Inserts `key`, or replaces its value while keeping `time_created` and refreshing `time_updated`.
pub fn set(db: &Connection, key: &str, value: &Value) -> Result<(), StorageError> {
    let text = encode_json(value);
    let now = now_ms();
    db.execute(
        "INSERT INTO kv (key, value, time_created, time_updated) VALUES (?, ?, ?, ?) ON CONFLICT (key) DO UPDATE SET value = ?, time_updated = ?",
        params![key, text, now, now, text, now],
    )?;
    Ok(())
}

/// Deletes `key`; a key that is not stored is not an error.
pub fn remove(db: &Connection, key: &str) -> Result<(), StorageError> {
    db.execute("DELETE FROM kv WHERE key = ?", [key])?;
    Ok(())
}

/// Reads up to `options.limit` keys in ascending order, reporting where to continue.
pub fn scan(db: &Connection, options: ScanOptions) -> Result<ScanResult, StorageError> {
    let limit = match options.limit {
        None => 100,
        Some(value) if value.is_nan() => 100,
        Some(value) => value.floor().clamp(1.0, 1000.0) as usize,
    };
    let end = prefix_end(&options.prefix);
    let mut conditions = Vec::new();
    let mut values: Vec<&dyn rusqlite::ToSql> = Vec::new();
    if !options.prefix.is_empty() {
        conditions.push("key >= ?");
        values.push(&options.prefix);
    }
    if let Some(end) = &end {
        conditions.push("key < ?");
        values.push(end);
    }
    if let Some(after) = &options.after {
        conditions.push("key > ?");
        values.push(after);
    }
    let fetch = limit as i64 + 1;
    values.push(&fetch);
    let mut sql = String::from("SELECT key, value FROM kv");
    if !conditions.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conditions.join(" AND "));
    }
    sql.push_str(" ORDER BY key ASC LIMIT ?");

    let mut statement = db.prepare(&sql)?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(values), |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut entries = rows
        .into_iter()
        .map(|(key, value)| Ok(Entry { key, value: serde_json::from_str(&value)? }))
        .collect::<Result<Vec<Entry>, StorageError>>()?;
    let next = (entries.len() > limit).then(|| entries[limit - 1].key.clone());
    entries.truncate(limit);
    Ok(ScanResult { entries, next })
}

/// The smallest key above every key that starts with `prefix`, or `None` when there is none.
///
/// `bun:sqlite` binds an incremented lone surrogate as the replacement character, so the same
/// substitution is applied here.
fn prefix_end(prefix: &str) -> Option<String> {
    let (index, last) = prefix.char_indices().rfind(|(_, value)| *value < char::MAX)?;
    let mut end = String::with_capacity(prefix.len() + 4);
    end.push_str(&prefix[..index]);
    end.push(char::from_u32(u32::from(last) + 1).unwrap_or('\u{FFFD}'));
    Some(end)
}
