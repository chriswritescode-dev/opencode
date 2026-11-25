use opencode_storage::{Database, StorageError};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params_from_iter};
use serde::{Deserialize, Serialize};

pub const VERSION: i64 = 1;
pub const MAX_TEXT: usize = 64 * 1024;
pub const MAX_CHUNK_BYTES: i64 = 1024 * 1024;
pub const CHUNK_LIMIT: usize = 50;

const STATE_KEY: &str = "session.search";
const COLUMNS: &str = "title, summary, user_text, assistant_text";
const TOKENIZER: &str = "tokenize='porter unicode61 remove_diacritics 2'";
const INDEXED: &str = "'user', 'assistant', 'compaction'";
const NAMES_TYPED: &str =
    "SELECT name FROM sqlite_master WHERE name GLOB 'session_search*' AND type IN ('table', 'view', 'trigger')";
const NAMES_ALL: &str = "SELECT name FROM sqlite_master WHERE name GLOB 'session_search*'";
const NAMES_TRIGGERS: &str = "SELECT name FROM sqlite_master WHERE type = 'trigger' AND name GLOB 'session_search*'";
const PROBE_SQL: &str = r#"SELECT json_extract('{"a":[1]}', '$.a'), json_valid('{}'), json_type('{}'), (SELECT count(*) FROM json_each('[1]'))"#;
const TABLES: [&str; 2] = ["session_search", "session_search_document"];

const FOLDS: [(&str, &str); 20] = [
    ("đ", "d"),
    ("Đ", "D"),
    ("ð", "d"),
    ("Ð", "D"),
    ("ł", "l"),
    ("Ł", "L"),
    ("ø", "o"),
    ("Ø", "O"),
    ("ħ", "h"),
    ("Ħ", "H"),
    ("ı", "i"),
    ("ŋ", "n"),
    ("Ŋ", "N"),
    ("ß", "ss"),
    ("æ", "ae"),
    ("Æ", "AE"),
    ("œ", "oe"),
    ("Œ", "OE"),
    ("þ", "th"),
    ("Þ", "TH"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    Compact,
    Text,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Titles,
    Messages,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Building,
    Done,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub version: i64,
    pub layout: Layout,
    pub status: Status,
    pub phase: Phase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    pub processed: i64,
    pub total: i64,
    pub skipped: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug)]
pub enum Ensured {
    Newer,
    Unsupported(String),
    Ready(State),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chunk {
    pub done: bool,
}

enum StoredState {
    Full(State),
    Version(i64),
}

impl StoredState {
    fn version(&self) -> i64 {
        match self {
            Self::Full(state) => state.version,
            Self::Version(version) => *version,
        }
    }

    fn into_state(self) -> Option<State> {
        match self {
            Self::Full(state) => Some(state),
            Self::Version(_) => None,
        }
    }
}

enum Picked {
    Layout(Layout),
    Unsupported(String),
}

struct Source {
    id: String,
    session: String,
}

struct Candidate {
    id: String,
    session: String,
    indexed: i64,
    bytes: i64,
}

pub fn ensure(database: &Database) -> Result<Ensured, StorageError> {
    let objects = database.read(|conn| names(conn, NAMES_TYPED))?;
    if let Some(reason) = probe(database, objects.iter().any(|name| name == "session_search"))? {
        return Ok(Ensured::Unsupported(reason));
    }
    let before = database.read(read_state)?;
    if before.as_ref().is_some_and(|state| state.version() > VERSION) {
        return Ok(Ensured::Newer);
    }
    if let Some(state) = current(&before)
        && expected(state.layout).iter().all(|name| objects.contains(name))
    {
        return Ok(Ensured::Ready(state));
    }
    let kept = match current(&before) {
        Some(state) if creatable(database, state.layout)?.is_none() => Some(state.layout),
        _ => None,
    };
    let layout = match kept {
        Some(layout) => layout,
        None => match pick(database)? {
            Picked::Layout(layout) => layout,
            Picked::Unsupported(reason) => return Ok(Ensured::Unsupported(reason)),
        },
    };
    database.transaction(TransactionBehavior::Immediate, |tx| -> Result<Ensured, StorageError> {
        let state = read_state(tx)?;
        if state.as_ref().is_some_and(|state| state.version() > VERSION) {
            return Ok(Ensured::Newer);
        }
        let present = names(tx, NAMES_ALL)?;
        if let Some(state) = current(&state)
            && state.layout == layout
            && expected(layout).iter().all(|name| present.contains(name))
        {
            return Ok(Ensured::Ready(state));
        }
        reset(tx, layout)?;
        let next = State {
            version: VERSION,
            layout,
            status: Status::Building,
            phase: Phase::Titles,
            after: None,
            processed: 0,
            total: total(tx)?,
            skipped: 0,
            error: None,
        };
        write_state(tx, &next)?;
        Ok(Ensured::Ready(next))
    })
}

pub fn build_chunk(database: &Database, limit: Option<usize>, skip: bool) -> Result<Chunk, StorageError> {
    let limit = limit.unwrap_or(CHUNK_LIMIT);
    database.transaction(TransactionBehavior::Immediate, |tx| -> Result<Chunk, StorageError> {
        let stored = read_state(tx)?;
        let Some(state) = current(&stored) else { return Ok(Chunk { done: true }) };
        if state.status != Status::Building {
            return Ok(Chunk { done: true });
        }
        if state.phase == Phase::Titles {
            let ids = session_ids(tx, state.after.as_deref(), limit)?;
            let last = ids.last().cloned();
            if !ids.is_empty() && !skip {
                let batch = ids.iter().map(|id| Source { id: id.clone(), session: id.clone() }).collect::<Vec<_>>();
                reindex_batch(tx, state.layout, &batch)?;
            }
            let mut next = state;
            let visited = ids.len() as i64;
            next.processed += visited;
            if skip {
                next.skipped += visited;
            }
            if ids.len() < limit {
                next.phase = Phase::Messages;
                next.after = None;
            } else {
                next.after = last;
            }
            write_state(tx, &next)?;
            return Ok(Chunk { done: false });
        }
        let size = if state.layout == Layout::Compact { "octet_length" } else { "length" };
        let before = if state.after.is_some() { "message.id < ?" } else { "1 = 1" };
        let sql = format!(
            "SELECT message.id, message.session_id AS session,
          CASE WHEN message.type IN ({INDEXED}) THEN 1 ELSE 0 END AS indexed,
          CASE WHEN message.type IN ({INDEXED}) THEN {size}(message.data) + coalesce((
            SELECT sum({size}(part.data)) FROM session_part AS part WHERE part.message_id = message.id
          ), 0) ELSE 0 END AS bytes
        FROM session_message AS message WHERE {before}
        ORDER BY message.id DESC LIMIT ?"
        );
        let mut values = Vec::new();
        if let Some(after) = &state.after {
            values.push(Value::Text(after.clone()));
        }
        values.push(Value::Integer(limit as i64));
        let mut statement = tx.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(values), |row| {
            Ok(Candidate { id: row.get(0)?, session: row.get(1)?, indexed: row.get(2)?, bytes: row.get(3)? })
        })?;
        let candidates = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        let count = candidates.len();
        let picked = budget(candidates);
        let last = picked.last().map(|row| row.id.clone());
        let indexed = picked
            .iter()
            .filter(|row| row.indexed == 1)
            .map(|row| Source { id: row.id.clone(), session: row.session.clone() })
            .collect::<Vec<_>>();
        if !indexed.is_empty() && !skip {
            reindex_batch(tx, state.layout, &indexed)?;
        }
        let done = last.is_none() || (count < limit && picked.len() == count);
        let mut next = state;
        let visited = picked.len() as i64;
        next.processed += visited;
        if skip {
            next.skipped += visited;
        }
        if done {
            next.status = Status::Done;
        } else {
            next.after = last;
        }
        write_state(tx, &next)?;
        Ok(Chunk { done })
    })
}

pub fn fail(database: &Database, error: &str) -> Result<(), StorageError> {
    update(database, |state| {
        (state.status == Status::Building).then(|| State {
            status: Status::Failed,
            error: Some(error.to_string()),
            ..state
        })
    })
}

pub fn retry(database: &Database) -> Result<(), StorageError> {
    update(database, |state| {
        (state.status == Status::Failed).then_some(State {
            status: Status::Building,
            phase: Phase::Titles,
            after: None,
            processed: 0,
            skipped: 0,
            error: None,
            ..state
        })
    })
}

pub fn state(database: &Database) -> Result<Option<State>, StorageError> {
    database.read(|conn| Ok(read_state(conn)?.and_then(StoredState::into_state)))
}

pub fn is_busy(error: &StorageError) -> bool {
    match error {
        StorageError::Sqlite(error) => matches!(
            error.sqlite_error_code(),
            Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
        ),
        _ => false,
    }
}

pub fn fold_text(value: &str) -> String {
    FOLDS.iter().fold(value.to_string(), |text, (from, to)| text.replace(from, to))
}

fn current(state: &Option<StoredState>) -> Option<State> {
    match state {
        Some(StoredState::Full(state)) if state.version == VERSION => Some(state.clone()),
        _ => None,
    }
}

fn read_state(conn: &Connection) -> Result<Option<StoredState>, StorageError> {
    let Some(value) = opencode_storage::kv::get(conn, STATE_KEY)? else { return Ok(None) };
    if let Ok(state) = serde_json::from_value::<State>(value.clone()) {
        return Ok(Some(StoredState::Full(state)));
    }
    Ok(value.get("version").and_then(|version| version.as_i64()).map(StoredState::Version))
}

fn write_state(conn: &Connection, state: &State) -> Result<(), StorageError> {
    opencode_storage::kv::set(conn, STATE_KEY, &serde_json::to_value(state)?)
}

fn update(database: &Database, change: impl FnOnce(State) -> Option<State>) -> Result<(), StorageError> {
    database.transaction(TransactionBehavior::Immediate, |tx| -> Result<(), StorageError> {
        let stored = read_state(tx)?;
        let Some(state) = current(&stored) else { return Ok(()) };
        if let Some(next) = change(state) {
            write_state(tx, &next)?;
        }
        Ok(())
    })
}

fn budget(rows: Vec<Candidate>) -> Vec<Candidate> {
    let mut picked = Vec::new();
    let mut bytes = 0;
    for row in rows {
        if !picked.is_empty() && bytes + row.bytes > MAX_CHUNK_BYTES {
            break;
        }
        bytes += row.bytes;
        picked.push(row);
    }
    picked
}

fn capability<T>(result: Result<T, StorageError>) -> Result<Option<String>, StorageError> {
    match result {
        Ok(_) => Ok(None),
        Err(error) => match capability_reason(&error) {
            Some(reason) => Ok(Some(reason)),
            None => Err(error),
        },
    }
}

fn capability_reason(error: &StorageError) -> Option<String> {
    let StorageError::Sqlite(error) = error else { return None };
    if error.sqlite_error_code() != Some(rusqlite::ErrorCode::Unknown) {
        return None;
    }
    Some(error.to_string())
}

fn probe(database: &Database, index: bool) -> Result<Option<String>, StorageError> {
    let json = capability(database.read(|conn| {
        conn.query_row(PROBE_SQL, [], |_| Ok(()))?;
        Ok(())
    }))?;
    if let Some(reason) = json {
        return Ok(Some(reason));
    }
    if !index {
        return Ok(None);
    }
    capability(database.read(|conn| {
        let mut statement = conn.prepare("SELECT rowid FROM session_search WHERE rowid = 0")?;
        let mut rows = statement.query([])?;
        rows.next()?;
        Ok(())
    }))
}

fn pick(database: &Database) -> Result<Picked, StorageError> {
    if creatable(database, Layout::Compact)?.is_none() {
        return Ok(Picked::Layout(Layout::Compact));
    }
    match creatable(database, Layout::Text)? {
        None => Ok(Picked::Layout(Layout::Text)),
        Some(reason) => Ok(Picked::Unsupported(reason)),
    }
}

fn creatable(database: &Database, layout: Layout) -> Result<Option<String>, StorageError> {
    capability(database.transaction(TransactionBehavior::Immediate, |tx| -> Result<(), StorageError> {
        tx.execute(&fts("session_search_probe", layout), [])?;
        tx.execute("DROP TABLE session_search_probe", [])?;
        Ok(())
    }))
}

fn total(conn: &Connection) -> Result<i64, StorageError> {
    let count = conn
        .prepare_cached("SELECT (SELECT count(*) FROM session_v2) + (SELECT count(*) FROM session_message) AS count")?
        .query_row([], |row| row.get::<_, i64>(0))
        .optional()?;
    Ok(count.unwrap_or(0))
}

fn names(conn: &Connection, sql: &str) -> Result<Vec<String>, StorageError> {
    let mut statement = conn.prepare(sql)?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(StorageError::from)
}

fn reset(conn: &Connection, layout: Layout) -> Result<(), StorageError> {
    unlink(conn)?;
    for table in TABLES {
        conn.execute(&format!("DROP TABLE IF EXISTS {table}"), [])?;
    }
    conn.execute(&document(layout), [])?;
    conn.execute(&fts("session_search", layout), [])?;
    link(conn, layout)
}

fn unlink(conn: &Connection) -> Result<(), StorageError> {
    for name in names(conn, NAMES_TRIGGERS)? {
        conn.execute(&format!("DROP TRIGGER {name}"), [])?;
    }
    conn.execute("DROP VIEW IF EXISTS session_search_source", [])?;
    Ok(())
}

fn link(conn: &Connection, layout: Layout) -> Result<(), StorageError> {
    conn.execute(&source_view(), [])?;
    for (name, body) in layout_triggers(layout).into_iter().chain(source_triggers(layout)) {
        conn.execute(&format!("CREATE TRIGGER {name} {body}"), [])?;
    }
    Ok(())
}

fn session_ids(conn: &Connection, after: Option<&str>, limit: usize) -> Result<Vec<String>, StorageError> {
    let sql = match after {
        Some(_) => "SELECT id FROM session_v2 WHERE id > ? ORDER BY id LIMIT ?",
        None => "SELECT id FROM session_v2 WHERE 1 = 1 ORDER BY id LIMIT ?",
    };
    let mut values = Vec::new();
    if let Some(after) = after {
        values.push(Value::Text(after.to_string()));
    }
    values.push(Value::Integer(limit as i64));
    let mut statement = conn.prepare(sql)?;
    let rows = statement.query_map(params_from_iter(values), |row| row.get::<_, String>(0))?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(StorageError::from)
}

fn reindex_batch(conn: &Connection, layout: Layout, rows: &[Source]) -> Result<(), StorageError> {
    for batch in rows.chunks(CHUNK_LIMIT) {
        let ids = batch.iter().map(|row| Value::Text(row.id.clone())).collect::<Vec<_>>();
        let placeholders = placeholders(batch.len());
        if layout == Layout::Text {
            conn.execute(
                &format!("DELETE FROM session_search_document WHERE source_id IN ({placeholders})"),
                params_from_iter(&ids),
            )?;
            conn.execute(
                &format!(
                    "INSERT INTO session_search_document (source_id, session_id, {COLUMNS})
        SELECT source_id, session_id, {COLUMNS} FROM session_search_source WHERE source_id IN ({placeholders})"
                ),
                params_from_iter(&ids),
            )?;
            continue;
        }
        conn.execute(
            &format!(
                "DELETE FROM session_search WHERE rowid IN (SELECT id FROM session_search_document WHERE source_id IN ({placeholders}))"
            ),
            params_from_iter(&ids),
        )?;
        conn.execute(
            &format!("DELETE FROM session_search_document WHERE source_id IN ({placeholders})"),
            params_from_iter(&ids),
        )?;
        let values_sql = batch.iter().map(|_| "(?, ?)").collect::<Vec<_>>().join(", ");
        let mut values = Vec::with_capacity(batch.len() * 2);
        for row in batch {
            values.push(Value::Text(row.id.clone()));
            values.push(Value::Text(row.session.clone()));
        }
        conn.execute(
            &format!("INSERT INTO session_search_document (source_id, session_id) VALUES {values_sql}"),
            params_from_iter(values),
        )?;
        conn.execute(
            &format!(
                "INSERT INTO session_search (rowid, {COLUMNS})
      SELECT document.id, source.title, source.summary, source.user_text, source.assistant_text
      FROM session_search_source AS source
      JOIN session_search_document AS document ON document.source_id = source.source_id
      WHERE source.source_id IN ({placeholders})"
            ),
            params_from_iter(&ids),
        )?;
    }
    Ok(())
}

fn placeholders(count: usize) -> String {
    vec!["?"; count].join(", ")
}

fn expected(layout: Layout) -> Vec<String> {
    let mut names = TABLES.iter().map(|name| (*name).to_string()).collect::<Vec<_>>();
    names.push("session_search_source".to_string());
    names.extend(source_triggers(layout).into_iter().map(|(name, _)| name));
    names.extend(layout_triggers(layout).into_iter().map(|(name, _)| name));
    names
}

fn source_view() -> String {
    format!(
        "CREATE VIEW session_search_source AS
SELECT source_id, session_id, {}, {}, {}, {} FROM (
  SELECT id AS source_id, id AS session_id, substr(title, 1, {MAX_TEXT}) AS title, NULL AS summary, NULL AS user_text, NULL AS assistant_text
  FROM session_v2 WHERE trim(coalesce(title, '')) <> ''
  UNION ALL
  SELECT m.id, m.session_id, NULL,
    CASE WHEN m.type = 'compaction' AND json_extract(m.data, '$.status') = 'completed'
      THEN nullif(substr(json_extract(m.data, '$.summary'), 1, {MAX_TEXT}), '') END,
    CASE WHEN m.type = 'user' THEN nullif(substr(json_extract(m.data, '$.text'), 1, {MAX_TEXT}), '') END,
    CASE WHEN m.type = 'assistant' AND json_extract(m.data, '$.time.completed') IS NOT NULL THEN substr((
      SELECT group_concat(t, char(10) || char(10)) FROM (
        SELECT 0 AS o, p.ordinal AS k, substr(json_extract(p.data, '$.text'), 1, {MAX_TEXT}) AS t
        FROM session_part AS p WHERE p.message_id = m.id AND p.type = 'text' AND json_valid(p.data)
        UNION ALL
        SELECT 1, c.key, substr(json_extract(m.data, c.fullkey || '.text'), 1, {MAX_TEXT})
        FROM json_each(m.data, '$.content') AS c WHERE json_extract(m.data, c.fullkey || '.type') = 'text'
        ORDER BY 1, 2
      ) WHERE trim(t) <> ''
    ), 1, {MAX_TEXT}) END
  FROM session_message AS m
  WHERE m.type IN ({INDEXED}) AND json_valid(m.data) AND json_type(m.data) = 'object'
) WHERE coalesce(title, summary, user_text, assistant_text) IS NOT NULL",
        fold("title"),
        fold("summary"),
        fold("user_text"),
        fold("assistant_text"),
    )
}

fn finished(row: &str) -> String {
    format!(
        "({row}.type <> 'assistant' OR NOT json_valid({row}.data) OR json_extract({row}.data, '$.time.completed') IS NOT NULL)"
    )
}

fn parent_finished(row: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM session_message AS m WHERE m.id = {row}.message_id AND m.type = 'assistant' AND json_valid(m.data) AND json_extract(m.data, '$.time.completed') IS NOT NULL)"
    )
}

fn remove(layout: Layout, id: &str) -> String {
    match layout {
        Layout::Compact => format!(
            "DELETE FROM session_search WHERE rowid = (SELECT id FROM session_search_document WHERE source_id = {id});
       DELETE FROM session_search_document WHERE source_id = {id};"
        ),
        Layout::Text => format!("DELETE FROM session_search_document WHERE source_id = {id};"),
    }
}

fn reindex(layout: Layout, id: &str, session: &str) -> String {
    match layout {
        Layout::Compact => format!(
            "{}
       INSERT INTO session_search_document (source_id, session_id) VALUES ({id}, {session});
       INSERT INTO session_search (rowid, {COLUMNS})
         SELECT document.id, source.title, source.summary, source.user_text, source.assistant_text
         FROM session_search_source AS source, session_search_document AS document
         WHERE source.source_id = {id} AND document.source_id = {id};",
            remove(layout, id)
        ),
        Layout::Text => format!(
            "DELETE FROM session_search_document WHERE source_id = {id};
       INSERT INTO session_search_document (source_id, session_id, {COLUMNS})
         SELECT source_id, session_id, {COLUMNS} FROM session_search_source WHERE source_id = {id};"
        ),
    }
}

fn source_triggers(layout: Layout) -> Vec<(String, String)> {
    vec![
        (
            "session_search_message_insert".to_string(),
            format!(
                "AFTER INSERT ON session_message WHEN NEW.type IN ({INDEXED}) AND {} BEGIN {} END",
                finished("NEW"),
                reindex(layout, "NEW.id", "NEW.session_id")
            ),
        ),
        (
            "session_search_message_update".to_string(),
            format!(
                "AFTER UPDATE OF type, data ON session_message WHEN (NEW.type IN ({INDEXED}) AND {}) OR (OLD.type IN ({INDEXED}) AND NEW.type NOT IN ({INDEXED})) BEGIN {} END",
                finished("NEW"),
                reindex(layout, "NEW.id", "NEW.session_id")
            ),
        ),
        (
            "session_search_message_delete".to_string(),
            format!(
                "AFTER DELETE ON session_message WHEN OLD.type IN ({INDEXED}) BEGIN {} END",
                remove(layout, "OLD.id")
            ),
        ),
        (
            "session_search_part_insert".to_string(),
            format!(
                "AFTER INSERT ON session_part WHEN NEW.type = 'text' AND {} BEGIN {} END",
                parent_finished("NEW"),
                reindex(layout, "NEW.message_id", "NEW.session_id")
            ),
        ),
        (
            "session_search_part_update".to_string(),
            format!(
                "AFTER UPDATE OF type, data, ordinal ON session_part WHEN (NEW.type = 'text' OR OLD.type = 'text') AND {} BEGIN {} END",
                parent_finished("NEW"),
                reindex(layout, "NEW.message_id", "NEW.session_id")
            ),
        ),
        (
            "session_search_part_delete".to_string(),
            format!(
                "AFTER DELETE ON session_part WHEN OLD.type = 'text' AND EXISTS (SELECT 1 FROM session_v2 AS s WHERE s.id = OLD.session_id) AND {} BEGIN {} END",
                parent_finished("OLD"),
                reindex(layout, "OLD.message_id", "OLD.session_id")
            ),
        ),
        (
            "session_search_session_insert".to_string(),
            format!(
                "AFTER INSERT ON session_v2 WHEN trim(coalesce(NEW.title, '')) <> '' BEGIN {} END",
                reindex(layout, "NEW.id", "NEW.id")
            ),
        ),
        (
            "session_search_session_update".to_string(),
            format!(
                "AFTER UPDATE OF title ON session_v2 WHEN NEW.title IS NOT OLD.title BEGIN {} END",
                reindex(layout, "NEW.id", "NEW.id")
            ),
        ),
        (
            "session_search_session_delete".to_string(),
            format!("AFTER DELETE ON session_v2 BEGIN {} END", remove(layout, "OLD.id")),
        ),
    ]
}

fn layout_triggers(layout: Layout) -> Vec<(String, String)> {
    match layout {
        Layout::Compact => vec![],
        Layout::Text => vec![
            (
                "session_search_document_insert".to_string(),
                format!(
                    "AFTER INSERT ON session_search_document BEGIN
        INSERT INTO session_search (rowid, {COLUMNS}) VALUES (NEW.id, NEW.title, NEW.summary, NEW.user_text, NEW.assistant_text);
      END"
                ),
            ),
            (
                "session_search_document_delete".to_string(),
                format!(
                    "AFTER DELETE ON session_search_document BEGIN
        INSERT INTO session_search (session_search, rowid, {COLUMNS}) VALUES ('delete', OLD.id, OLD.title, OLD.summary, OLD.user_text, OLD.assistant_text);
      END"
                ),
            ),
        ],
    }
}

fn fts(name: &str, layout: Layout) -> String {
    match layout {
        Layout::Compact => {
            format!("CREATE VIRTUAL TABLE {name} USING fts5({COLUMNS}, content='', contentless_delete=1, {TOKENIZER})")
        }
        Layout::Text => format!(
            "CREATE VIRTUAL TABLE {name} USING fts5({COLUMNS}, content='session_search_document', content_rowid='id', {TOKENIZER})"
        ),
    }
}

fn document(layout: Layout) -> String {
    match layout {
        Layout::Compact => "CREATE TABLE session_search_document (id INTEGER PRIMARY KEY, source_id TEXT NOT NULL UNIQUE, session_id TEXT NOT NULL)".to_string(),
        Layout::Text => format!(
            "CREATE TABLE session_search_document (id INTEGER PRIMARY KEY, source_id TEXT NOT NULL UNIQUE, session_id TEXT NOT NULL, {COLUMNS})"
        ),
    }
}

fn fold(column: &str) -> String {
    let mut text = column.to_string();
    for (from, to) in FOLDS {
        text = format!("replace({text}, '{from}', '{to}')");
    }
    format!("{text} AS {column}")
}
