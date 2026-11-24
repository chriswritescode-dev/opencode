use std::sync::Arc;

use opencode_session::{Cursor, Direction, MessagesInput, SessionStore};
use opencode_storage::{Database, DatabaseLocation};

use crate::TempDir;

#[test]
fn get_returns_none_for_a_missing_session() {
    let dir = TempDir::new();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    let store = SessionStore::new(Arc::new(database));
    assert!(store.get("ses_missing").expect("read the session").is_none());
}

#[test]
fn an_unknown_cursor_returns_no_messages() {
    let dir = TempDir::new();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    database
        .read(|connection| {
            connection.execute(
                "INSERT INTO project (id, worktree, sandboxes, time_created, time_updated) \
                 VALUES ('prj_1', '/repo', '[]', 1, 2)",
                [],
            )?;
            connection.execute(
                "INSERT INTO session_v2 (id, project_id, slug, directory, version, time_created, time_updated) \
                 VALUES ('ses_1', 'prj_1', 'slug-1', '/repo', '1', 1000, 1000)",
                [],
            )?;
            connection.execute(
                "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data) \
                 VALUES ('msg_1', 'ses_1', 'assistant', 0, 1000, 1000, \
                 '{\"time\":{\"created\":1000},\"agent\":\"build\",\"model\":{\"id\":\"gpt-5\",\"providerID\":\"openai\"},\"content\":[],\"type\":\"assistant\"}')",
                [],
            )?;
            Ok(())
        })
        .expect("seed the session");

    let store = SessionStore::new(Arc::new(database));
    let messages = store
        .messages(&MessagesInput {
            session_id: "ses_1".to_string(),
            limit: None,
            order: None,
            r#type: None,
            cursor: Some(Cursor { id: "msg_missing".to_string(), direction: Direction::Next }),
        })
        .expect("read the messages");
    assert!(messages.is_empty());
}
