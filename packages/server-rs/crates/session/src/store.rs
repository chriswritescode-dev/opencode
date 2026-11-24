use std::sync::Arc;

use opencode_bus::BusError;
use opencode_contract::generated::{SessionInfo, SessionMessageAssistant, SessionMessageInfo, SessionMessageMediaInfo};
use opencode_storage::{Database, StorageError};
use rusqlite::types::Value;
use rusqlite::{OptionalExtension, params, params_from_iter};

use crate::history::{self, HistoryError, MessageDecodeError, MessageRow};
use crate::info;
use crate::sql::now_millis;

const MESSAGE_COLUMNS: &str = "id, session_id, type, seq, data";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Order {
    Asc,
    Desc,
}

impl Order {
    fn flip(self) -> Self {
        match self {
            Self::Asc => Self::Desc,
            Self::Desc => Self::Asc,
        }
    }

    fn keyword(self) -> &'static str {
        match self {
            Self::Asc => "ASC",
            Self::Desc => "DESC",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Previous,
    Next,
}

pub struct ListAnchor {
    pub id: String,
    pub time: f64,
    pub direction: Direction,
}

pub struct ListInput {
    pub directory: Option<String>,
    pub workspace_id: Option<String>,
    pub project: Option<String>,
    pub subpath: Option<String>,
    pub search: Option<String>,
    pub limit: Option<i64>,
    pub order: Option<Order>,
    pub parent_id: Option<Option<String>>,
    pub archived: Option<bool>,
    pub anchor: Option<ListAnchor>,
}

pub struct Cursor {
    pub id: String,
    pub direction: Direction,
}

pub struct MessagesInput {
    pub session_id: String,
    pub limit: Option<i64>,
    pub order: Option<Order>,
    pub r#type: Option<String>,
    pub cursor: Option<Cursor>,
}

pub struct LastAssistantInput {
    pub session_id: String,
    pub after_seq: Option<i64>,
}

pub enum SessionMessage {
    Info(SessionMessageInfo),
    Media(SessionMessageMediaInfo),
}

pub type Overlay = Arc<dyn Fn(&mut Vec<SessionMessage>) + Send + Sync>;

pub struct SessionStore {
    db: Arc<Database>,
    overlay: Overlay,
}

impl SessionStore {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db, overlay: Arc::new(|_| {}) }
    }

    pub fn with_overlay(db: Arc<Database>, overlay: Overlay) -> Self {
        Self { db, overlay }
    }

    pub fn get(&self, session_id: &str) -> Result<Option<SessionInfo>, BusError> {
        let sql = format!("SELECT {} FROM session_v2 WHERE id = ?1", info::COLUMNS);
        let row = self
            .db
            .read(move |connection| Ok(connection.query_row(&sql, [session_id], info::read).optional()?))
            .map_err(BusError::from)?;
        row.as_ref().map(info::from_row).transpose()
    }

    pub fn list(&self, input: &ListInput) -> Result<Vec<SessionInfo>, BusError> {
        let direction = input.anchor.as_ref().map_or(Direction::Next, |anchor| anchor.direction);
        let requested = input.order.unwrap_or(Order::Desc);
        let order = if direction == Direction::Previous { requested.flip() } else { requested };

        let mut sql = format!("SELECT {} FROM session_v2", info::COLUMNS);
        let mut conditions: Vec<String> = Vec::new();
        let mut values: Vec<Value> = Vec::new();
        if let Some(directory) = &input.directory {
            values.push(Value::Text(directory.clone()));
            conditions.push(format!("directory = ?{}", values.len()));
        }
        if let Some(workspace_id) = input.workspace_id.as_ref().filter(|value| !value.is_empty()) {
            values.push(Value::Text(workspace_id.clone()));
            conditions.push(format!("workspace_id = ?{}", values.len()));
        }
        if let Some(project) = &input.project {
            values.push(Value::Text(project.clone()));
            conditions.push(format!("project_id = ?{}", values.len()));
            if let Some(subpath) = &input.subpath {
                values.push(Value::Text(subpath.clone()));
                conditions.push(format!("path = ?{}", values.len()));
            }
        }
        if let Some(search) = input.search.as_ref().filter(|value| !value.is_empty()) {
            values.push(Value::Text(title_pattern(search)));
            conditions.push(format!("title GLOB ?{}", values.len()));
        }
        match &input.parent_id {
            Some(None) => conditions.push("parent_id IS NULL".to_string()),
            Some(Some(parent_id)) => {
                values.push(Value::Text(parent_id.clone()));
                conditions.push(format!("parent_id = ?{}", values.len()));
            }
            None => {}
        }
        if let Some(archived) = input.archived {
            conditions.push(if archived { "time_archived IS NOT NULL" } else { "time_archived IS NULL" }.to_string());
        }
        if let Some(anchor) = &input.anchor {
            let comparison = if order == Order::Asc { ">" } else { "<" };
            let time = values.len() + 1;
            conditions.push(format!(
                "(time_updated {comparison} ?{time} OR (time_updated = ?{time} AND id {comparison} ?{}))",
                time + 1
            ));
            values.push(Value::Real(anchor.time));
            values.push(Value::Text(anchor.id.clone()));
        }
        if !conditions.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conditions.join(" AND "));
        }
        sql.push_str(&format!(" ORDER BY time_updated {}, id {}", order.keyword(), order.keyword()));
        if let Some(limit) = input.limit {
            values.push(Value::Integer(limit));
            sql.push_str(&format!(" LIMIT ?{}", values.len()));
        }

        let mut rows = self
            .db
            .read(move |connection| {
                let mut statement = connection.prepare(&sql)?;
                let mut rows = statement.query(params_from_iter(values))?;
                let mut stored = Vec::new();
                while let Some(row) = rows.next()? {
                    stored.push(info::read(row)?);
                }
                Ok(stored)
            })
            .map_err(BusError::from)?;
        if direction == Direction::Previous {
            rows.reverse();
        }
        rows.iter().map(info::from_row).collect()
    }

    pub fn messages(&self, input: &MessagesInput) -> Result<Vec<SessionMessageInfo>, HistoryError> {
        let rows = self.message_rows(input)?;
        let decoded = self.db.read(|connection| Ok(history::decode_message_rows(connection, &rows)))?;
        Ok(self.overlay_info(decoded?))
    }

    pub fn messages_with_refs(&self, input: &MessagesInput) -> Result<Vec<SessionMessageMediaInfo>, HistoryError> {
        let rows = self.message_rows(input)?;
        let decoded = self.db.read(|connection| Ok(history::decode_message_rows_with_refs(connection, &rows)))?;
        Ok(self.overlay_media(decoded?))
    }

    pub fn message(&self, message_id: &str) -> Result<Option<(String, SessionMessageInfo)>, HistoryError> {
        let Some(row) = self.message_row(message_id)? else {
            return Ok(None);
        };
        let session_id = row.session_id.clone();
        let decoded =
            self.db.read(|connection| Ok(history::decode_message_rows(connection, std::slice::from_ref(&row))))?;
        Ok(self.overlay_info(decoded?).into_iter().next().map(|message| (session_id, message)))
    }

    pub fn message_with_refs(
        &self,
        message_id: &str,
    ) -> Result<Option<(String, SessionMessageMediaInfo)>, HistoryError> {
        let Some(row) = self.message_row(message_id)? else {
            return Ok(None);
        };
        let session_id = row.session_id.clone();
        let decoded = self
            .db
            .read(|connection| Ok(history::decode_message_rows_with_refs(connection, std::slice::from_ref(&row))))?;
        Ok(self.overlay_media(decoded?).into_iter().next().map(|message| (session_id, message)))
    }

    pub fn last_assistant_after(
        &self,
        input: &LastAssistantInput,
    ) -> Result<Option<SessionMessageAssistant>, HistoryError> {
        let session_id = input.session_id.clone();
        let after_seq = input.after_seq;
        let mut cursor_seq = None;
        loop {
            let stored = self.db.read({
                let session_id = session_id.clone();
                move |connection| {
                    let mut conditions = vec!["session_id = ?1".to_string(), "type = 'assistant'".to_string()];
                    let mut values = vec![Value::Text(session_id)];
                    if let Some(after_seq) = after_seq {
                        values.push(Value::Integer(after_seq));
                        conditions.push(format!("seq > ?{}", values.len()));
                    }
                    if let Some(cursor_seq) = cursor_seq {
                        values.push(Value::Integer(cursor_seq));
                        conditions.push(format!("seq < ?{}", values.len()));
                    }
                    let sql = format!(
                        "SELECT {MESSAGE_COLUMNS} FROM session_message WHERE {} ORDER BY seq DESC LIMIT 20",
                        conditions.join(" AND ")
                    );
                    let mut statement = connection.prepare(&sql)?;
                    Ok(stored_rows(&mut statement, values)?)
                }
            })?;
            let count = stored.len();
            let rows = stored.into_iter().map(StoredMessage::decode).collect::<Result<Vec<_>, _>>()?;
            let next_cursor = rows.last().map(|row| row.seq);
            let decoded = self.db.read(|connection| Ok(history::decode_message_rows(connection, &rows)))?;
            for message in decoded? {
                if let SessionMessageInfo::Assistant(assistant) = message
                    && assistant.time.completed.is_some()
                    && assistant.error.is_none()
                {
                    return Ok(Some(assistant));
                }
            }
            if count < 20 {
                return Ok(None);
            }
            cursor_seq = next_cursor;
        }
    }

    pub fn list_suspended(&self) -> Result<Vec<String>, BusError> {
        self.db
            .read(|connection| {
                let mut statement = connection.prepare("SELECT id FROM session_v2 WHERE time_suspended IS NOT NULL")?;
                Ok(statement.query_map([], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?)
            })
            .map_err(BusError::from)
    }

    pub fn claim(&self, session_id: &str) -> Result<(), BusError> {
        self.db
            .read(|connection| {
                connection.execute(
                    "UPDATE session_v2 SET time_suspended = ?1, time_updated = time_updated \
                     WHERE id = ?2 AND time_suspended IS NULL",
                    params![now_millis(), session_id],
                )?;
                Ok(())
            })
            .map_err(BusError::from)
    }

    pub fn release(&self, session_id: &str) -> Result<(), BusError> {
        self.db
            .read(|connection| {
                connection.execute(
                    "UPDATE session_v2 SET time_suspended = NULL, resume_attempts = 0, \
                     time_updated = time_updated WHERE id = ?1",
                    [session_id],
                )?;
                Ok(())
            })
            .map_err(BusError::from)
    }

    pub fn release_orphaned_child_claims(&self) -> Result<(), BusError> {
        self.db
            .read(|connection| {
                connection.execute(
                    "UPDATE session_v2 SET time_suspended = NULL, resume_attempts = 0, \
                     time_updated = time_updated WHERE time_suspended IS NOT NULL \
                     AND parent_id NOT IN (SELECT id FROM session_v2)",
                    [],
                )?;
                Ok(())
            })
            .map_err(BusError::from)
    }

    pub fn count_resume(&self, session_id: &str) -> Result<Option<i64>, BusError> {
        self.db
            .read(|connection| {
                Ok(connection
                    .query_row(
                        "UPDATE session_v2 SET resume_attempts = resume_attempts + 1, \
                         time_updated = time_updated WHERE id = ?1 RETURNING resume_attempts",
                        [session_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?)
            })
            .map_err(BusError::from)
    }

    fn message_rows(&self, input: &MessagesInput) -> Result<Vec<MessageRow>, HistoryError> {
        let direction = input.cursor.as_ref().map_or(Direction::Next, |cursor| cursor.direction);
        let requested = input.order.unwrap_or(Order::Desc);
        let order = if direction == Direction::Previous { requested.flip() } else { requested };
        let session_id = input.session_id.clone();
        let cursor_id = input.cursor.as_ref().map(|cursor| cursor.id.clone());
        let filter_type = input.r#type.clone();
        let limit = input.limit;

        let stored = self.db.read(move |connection| {
            let anchor = match &cursor_id {
                Some(cursor_id) => {
                    let seq = connection
                        .query_row(
                            "SELECT seq FROM session_message WHERE session_id = ?1 AND id = ?2",
                            params![session_id, cursor_id],
                            |row| row.get::<_, i64>(0),
                        )
                        .optional()?;
                    match seq {
                        Some(seq) => Some(seq),
                        None => return Ok(Vec::new()),
                    }
                }
                None => None,
            };
            let mut conditions = vec!["session_id = ?1".to_string()];
            let mut values = vec![Value::Text(session_id)];
            if let Some(anchor) = anchor {
                let comparison = if order == Order::Asc { ">" } else { "<" };
                values.push(Value::Integer(anchor));
                conditions.push(format!("seq {comparison} ?{}", values.len()));
            }
            if let Some(filter_type) = filter_type {
                values.push(Value::Text(filter_type));
                conditions.push(format!("type = ?{}", values.len()));
            }
            let mut sql = format!(
                "SELECT {MESSAGE_COLUMNS} FROM session_message WHERE {} ORDER BY seq {}",
                conditions.join(" AND "),
                order.keyword()
            );
            if let Some(limit) = limit {
                values.push(Value::Integer(limit));
                sql.push_str(&format!(" LIMIT ?{}", values.len()));
            }
            let mut statement = connection.prepare(&sql)?;
            Ok(stored_rows(&mut statement, values)?)
        })?;
        let mut rows = stored.into_iter().map(StoredMessage::decode).collect::<Result<Vec<_>, _>>()?;
        if direction == Direction::Previous {
            rows.reverse();
        }
        Ok(rows)
    }

    fn message_row(&self, message_id: &str) -> Result<Option<MessageRow>, HistoryError> {
        let sql = format!("SELECT {MESSAGE_COLUMNS} FROM session_message WHERE id = ?1");
        let stored = self.db.read(move |connection| {
            let mut statement = connection.prepare(&sql)?;
            Ok(stored_rows(&mut statement, vec![Value::Text(message_id.to_string())])?)
        })?;
        stored.into_iter().next().map(StoredMessage::decode).transpose()
    }

    fn overlay_info(&self, messages: Vec<SessionMessageInfo>) -> Vec<SessionMessageInfo> {
        self.apply_overlay(messages.into_iter().map(SessionMessage::Info).collect())
            .into_iter()
            .filter_map(|message| match message {
                SessionMessage::Info(message) => Some(message),
                SessionMessage::Media(_) => None,
            })
            .collect()
    }

    fn overlay_media(&self, messages: Vec<SessionMessageMediaInfo>) -> Vec<SessionMessageMediaInfo> {
        self.apply_overlay(messages.into_iter().map(SessionMessage::Media).collect())
            .into_iter()
            .filter_map(|message| match message {
                SessionMessage::Media(message) => Some(message),
                SessionMessage::Info(_) => None,
            })
            .collect()
    }

    fn apply_overlay(&self, mut messages: Vec<SessionMessage>) -> Vec<SessionMessage> {
        (self.overlay)(&mut messages);
        messages
    }
}

struct StoredMessage {
    id: String,
    session_id: String,
    r#type: String,
    seq: i64,
    data: String,
}

impl StoredMessage {
    fn decode(self) -> Result<MessageRow, HistoryError> {
        let data = serde_json::from_str(&self.data).map_err(|_| {
            HistoryError::Decode(MessageDecodeError {
                session_id: self.session_id.clone(),
                message_id: self.id.clone(),
            })
        })?;
        Ok(MessageRow { id: self.id, session_id: self.session_id, seq: self.seq, r#type: self.r#type, data })
    }
}

fn stored_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredMessage> {
    Ok(StoredMessage {
        id: row.get(0)?,
        session_id: row.get(1)?,
        r#type: row.get(2)?,
        seq: row.get(3)?,
        data: row.get(4)?,
    })
}

fn stored_rows(statement: &mut rusqlite::Statement<'_>, values: Vec<Value>) -> rusqlite::Result<Vec<StoredMessage>> {
    statement.query_map(params_from_iter(values), stored_row)?.collect()
}

impl From<StorageError> for HistoryError {
    fn from(error: StorageError) -> Self {
        Self::Database(error.into())
    }
}

fn title_pattern(search: &str) -> String {
    let mut chars = String::new();
    for char in search.chars() {
        if char == '%' {
            chars.push('*');
            continue;
        }
        if char == '_' {
            chars.push('?');
            continue;
        }
        let mut forms: Vec<String> = Vec::new();
        for form in [char.to_string(), char.to_lowercase().collect::<String>(), char.to_uppercase().collect::<String>()]
        {
            if form.chars().count() == 1 && !forms.contains(&form) {
                forms.push(form);
            }
        }
        if forms.len() > 1 {
            chars.push('[');
            chars.push_str(&forms.concat());
            chars.push(']');
        } else if char == '*' || char == '?' || char == '[' {
            chars.push('[');
            chars.push(char);
            chars.push(']');
        } else {
            chars.push(char);
        }
    }
    format!("*{chars}*")
}
