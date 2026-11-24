use std::collections::BTreeMap;

use rusqlite::{Connection, Row};
use serde_json::{Value, json};

use crate::StorageError;

pub type Masks = BTreeMap<String, Vec<String>>;

pub fn dump(conn: &Connection, masks: &Masks) -> Result<Value, StorageError> {
    let mut output = serde_json::Map::new();
    output.insert("schema".to_owned(), schema(conn)?);
    output.insert("tables".to_owned(), tables(conn, masks)?);
    output.insert("foreignKeyViolations".to_owned(), foreign_key_violations(conn)?);
    Ok(Value::Object(output))
}

fn schema(conn: &Connection) -> Result<Value, StorageError> {
    let mut statement = conn.prepare("SELECT type, name, tbl_name, sql FROM sqlite_master ORDER BY type, name")?;
    let rows = statement.query_map([], |row| {
        let sql: Option<String> = row.get(3)?;
        Ok(json!([
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            sql.map_or(Value::Null, Value::String),
        ]))
    })?;
    Ok(Value::Array(rows.collect::<Result<Vec<_>, _>>()?))
}

fn tables(conn: &Connection, masks: &Masks) -> Result<Value, StorageError> {
    let mut statement = conn.prepare(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND substr(name, 1, 7) <> 'sqlite_' ORDER BY name",
    )?;
    let names = statement.query_map([], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
    let mut tables = BTreeMap::new();
    for name in names {
        let masked = masks.get(&name).map_or(&[][..], Vec::as_slice);
        tables.insert(name.clone(), table(conn, &name, masked)?);
    }
    Ok(Value::Object(tables.into_iter().collect()))
}

fn table(conn: &Connection, name: &str, masked: &[String]) -> Result<Value, StorageError> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({})", quote(name)))?;
    let columns = statement.query_map([], |row| row.get::<_, String>(1))?.collect::<Result<Vec<_>, _>>()?;
    let rows = rows(conn, name, &columns, masked)?;
    let mut table = serde_json::Map::new();
    table.insert("columns".to_owned(), Value::Array(columns.into_iter().map(Value::String).collect()));
    table.insert("rows".to_owned(), rows);
    Ok(Value::Object(table))
}

fn rows(conn: &Connection, table: &str, columns: &[String], masked: &[String]) -> Result<Value, StorageError> {
    if columns.is_empty() {
        return Ok(Value::Array(Vec::new()));
    }
    let projection = columns
        .iter()
        .map(|column| {
            let column = quote(column);
            format!("typeof({column}), CASE typeof({column}) WHEN 'blob' THEN hex({column}) ELSE {column} END")
        })
        .collect::<Vec<_>>()
        .join(", ");
    let mut statement = conn.prepare(&format!("SELECT {projection} FROM {}", quote(table)))?;
    let mut query = statement.query([])?;
    let mut rows = Vec::new();
    while let Some(row) = query.next()? {
        let mut cells = Vec::with_capacity(columns.len());
        for (index, column) in columns.iter().enumerate() {
            cells.push(cell(row, index * 2, masked.contains(column))?);
        }
        rows.push(Value::Array(cells));
    }
    rows.sort_by_cached_key(|row| serde_json::to_string(row).unwrap_or_default());
    Ok(Value::Array(rows))
}

fn cell(row: &Row<'_>, index: usize, masked: bool) -> rusqlite::Result<Value> {
    let kind: String = row.get(index)?;
    if kind == "null" {
        return Ok(Value::Null);
    }
    if masked {
        return Ok(Value::String("<masked>".to_owned()));
    }
    Ok(match kind.as_str() {
        "integer" => json!(["integer", row.get::<_, i64>(index + 1)?.to_string()]),
        "real" => json!(["real", opencode_api_types::js::number_to_string(row.get(index + 1)?)]),
        "text" => {
            let value = row.get_ref(index + 1)?;
            json!(["text", String::from_utf8_lossy(value.as_bytes()?).into_owned()])
        }
        "blob" => json!(["blob", row.get::<_, String>(index + 1)?]),
        _ => Value::Null,
    })
}

fn foreign_key_violations(conn: &Connection) -> Result<Value, StorageError> {
    let mut statement = conn.prepare("PRAGMA foreign_key_check")?;
    let violations = statement.query_map([], |_| Ok(()))?.collect::<Result<Vec<_>, _>>()?.len();
    Ok(Value::from(violations))
}

fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}
