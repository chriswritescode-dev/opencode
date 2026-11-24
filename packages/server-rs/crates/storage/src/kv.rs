use opencode_contract::codec::encode_json;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

use crate::StorageError;
use crate::id::now_ms;

pub struct Entry {
    pub key: String,
    pub value: Value,
}

pub struct ScanOptions {
    pub prefix: String,
    pub after: Option<String>,
    pub limit: Option<f64>,
}

pub struct ScanResult {
    pub entries: Vec<Entry>,
    pub next: Option<String>,
}

pub fn get(db: &Connection, key: &str) -> Result<Option<Value>, StorageError> {
    let value = db
        .prepare_cached("SELECT value FROM kv WHERE key = ?")?
        .query_row([key], |row| row.get::<_, String>(0))
        .optional()?;
    match value {
        Some(text) => Ok(Some(serde_json::from_str(&text)?)),
        None => Ok(None),
    }
}

pub fn set(db: &Connection, key: &str, value: &Value) -> Result<(), StorageError> {
    let text = encode_json(value);
    let now = now_ms();
    db.prepare_cached(
        "INSERT INTO kv (key, value, time_created, time_updated) VALUES (?, ?, ?, ?) ON CONFLICT (key) DO UPDATE SET value = ?, time_updated = ?",
    )?
    .execute(params![key, text, now, now, text, now])?;
    Ok(())
}

pub fn remove(db: &Connection, key: &str) -> Result<(), StorageError> {
    db.prepare_cached("DELETE FROM kv WHERE key = ?")?.execute([key])?;
    Ok(())
}

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

    let rows = db
        .prepare_cached(&sql)?
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

fn prefix_end(prefix: &str) -> Option<String> {
    let (index, last) = prefix.char_indices().rfind(|(_, value)| *value < char::MAX)?;
    let mut end = String::with_capacity(prefix.len() + 4);
    end.push_str(&prefix[..index]);
    end.push(char::from_u32(u32::from(last) + 1).unwrap_or('\u{FFFD}'));
    Some(end)
}
