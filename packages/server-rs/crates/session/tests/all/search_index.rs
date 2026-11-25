use opencode_session::search_index::{self, Ensured, Layout, Phase, Status, VERSION};
use opencode_storage::{Database, DatabaseLocation, kv};

use crate::TempDir;

fn open_database() -> (TempDir, Database) {
    let dir = TempDir::new();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    (dir, database)
}

fn ensure(database: &Database) -> search_index::State {
    match search_index::ensure(database).expect("ensure the search index") {
        Ensured::Ready(state) => state,
        other => panic!("expected a ready search index, got {other:?}"),
    }
}

fn drain(database: &Database) {
    for _ in 0..10_000 {
        if search_index::build_chunk(database, None, false).expect("build a search chunk").done {
            return;
        }
    }
    panic!("the search index build did not finish");
}

fn count(database: &Database, sql: &str) -> i64 {
    database
        .read(|connection| Ok(connection.query_row(sql, [], |row| row.get::<_, i64>(0))?))
        .expect("count the search rows")
}

fn object_exists(database: &Database, kind: &str, name: &str) -> bool {
    database
        .read(|connection| {
            let count: i64 = connection.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = ?1 AND name = ?2",
                [kind, name],
                |row| row.get(0),
            )?;
            Ok(count > 0)
        })
        .expect("read sqlite_master")
}

fn document_exists(database: &Database, source_id: &str, session_id: &str) -> bool {
    database
        .read(|connection| {
            let count: i64 = connection.query_row(
                "SELECT count(*) FROM session_search_document WHERE source_id = ?1 AND session_id = ?2",
                [source_id, session_id],
                |row| row.get(0),
            )?;
            Ok(count > 0)
        })
        .expect("read the search document")
}

fn match_count(database: &Database, term: &str) -> i64 {
    database
        .read(|connection| {
            let count: i64 = connection.query_row(
                "SELECT count(*) FROM session_search WHERE session_search MATCH ?1",
                [term],
                |row| row.get(0),
            )?;
            Ok(count)
        })
        .expect("count the search matches")
}

fn schema(database: &Database) -> Vec<(String, String, Option<String>)> {
    database
        .read(|connection| {
            let mut statement = connection.prepare("SELECT type, name, sql FROM sqlite_master ORDER BY type, name")?;
            let rows = statement.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?))
            })?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .expect("read the schema")
}

fn seed_session(database: &Database, title: Option<&str>) {
    database
        .read(|connection| {
            connection.execute(
                "INSERT INTO project (id, worktree, sandboxes, time_created, time_updated) VALUES ('prj_1', '/repo', '[]', 1, 2)",
                [],
            )?;
            connection.execute(
                "INSERT INTO session_v2 (id, project_id, slug, directory, version, title, time_created, time_updated) \
                 VALUES ('ses_1', 'prj_1', 'slug-1', '/repo', '1', ?1, 1000, 1000)",
                [title],
            )?;
            Ok(())
        })
        .expect("seed the project and session");
}

fn insert_message(database: &Database, id: &str, kind: &str, seq: i64, data: &str) {
    database
        .read(|connection| {
            connection.execute(
                "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data) \
                 VALUES (?1, 'ses_1', ?2, ?3, 1000, 1000, ?4)",
                (id, kind, seq, data),
            )?;
            Ok(())
        })
        .expect("insert the session message");
}

fn insert_text_part(database: &Database, message_id: &str, text: &str) {
    let data = serde_json::json!({ "text": text, "type": "text" }).to_string();
    database
        .read(|connection| {
            connection.execute(
                "INSERT INTO session_part (message_id, session_id, ordinal, type, data, time_created, time_updated) \
                 VALUES (?1, 'ses_1', 0, 'text', ?2, 1000, 1000)",
                (message_id, data),
            )?;
            Ok(())
        })
        .expect("insert the session part");
}

#[test]
fn ensure_builds_the_schema_and_finishes_on_an_empty_database() {
    let (_dir, database) = open_database();

    let state = ensure(&database);
    assert_eq!(state.version, VERSION);
    assert_eq!(state.layout, Layout::Compact);
    assert_eq!(state.status, Status::Building);
    assert_eq!(state.phase, Phase::Titles);
    assert_eq!(state.after, None);
    assert_eq!(state.processed, 0);
    assert_eq!(state.total, 0);
    assert_eq!(state.skipped, 0);

    assert!(object_exists(&database, "table", "session_search"));
    assert!(object_exists(&database, "table", "session_search_document"));
    assert!(object_exists(&database, "view", "session_search_source"));
    for trigger in [
        "session_search_message_insert",
        "session_search_message_update",
        "session_search_message_delete",
        "session_search_part_insert",
        "session_search_part_update",
        "session_search_part_delete",
        "session_search_session_insert",
        "session_search_session_update",
        "session_search_session_delete",
    ] {
        assert!(object_exists(&database, "trigger", trigger), "missing the {trigger} trigger");
    }

    drain(&database);

    let state = search_index::state(&database).expect("read the search state").expect("the search state");
    assert_eq!(state.status, Status::Done);
    assert_eq!(state.phase, Phase::Messages);
    assert_eq!(state.processed, 0);
    assert_eq!(state.total, 0);
    assert_eq!(state.skipped, 0);
    assert_eq!(state.after, None);
    assert_eq!(state.error, None);

    let raw = database
        .read(|connection| kv::get(connection, "session.search"))
        .expect("read the search state row")
        .expect("the search state row");
    assert_eq!(
        raw,
        serde_json::json!({
            "version": 1,
            "layout": "compact",
            "status": "done",
            "phase": "messages",
            "processed": 0,
            "total": 0,
            "skipped": 0,
        })
    );
}

#[test]
fn triggers_index_titles_and_finished_messages() {
    let (_dir, database) = open_database();
    ensure(&database);
    drain(&database);

    seed_session(&database, Some("Search title"));
    assert!(document_exists(&database, "ses_1", "ses_1"));

    insert_message(&database, "msg_user", "user", 1, r#"{"text":"hello search","type":"user"}"#);
    assert!(document_exists(&database, "msg_user", "ses_1"));
    assert_eq!(match_count(&database, "hello"), 1);

    insert_message(&database, "msg_assistant", "assistant", 2, r#"{"type":"assistant","time":{"created":1}}"#);
    insert_text_part(&database, "msg_assistant", "unfinished answer");
    assert!(!document_exists(&database, "msg_assistant", "ses_1"));

    database
        .read(|connection| {
            connection.execute(
                "UPDATE session_message SET data = ?1 WHERE id = 'msg_assistant'",
                [r#"{"type":"assistant","time":{"completed":2}}"#],
            )?;
            Ok(())
        })
        .expect("finish the assistant message");
    assert!(document_exists(&database, "msg_assistant", "ses_1"));
    assert_eq!(match_count(&database, "unfinished"), 1);

    database
        .read(|connection| {
            connection.execute("DELETE FROM session_message WHERE id = 'msg_user'", [])?;
            Ok(())
        })
        .expect("delete the user message");
    assert!(!document_exists(&database, "msg_user", "ses_1"));
}

#[test]
fn build_indexes_rows_that_existed_before_ensure() {
    let (_dir, database) = open_database();

    seed_session(&database, Some("Existing title"));
    insert_message(&database, "msg_user", "user", 1, r#"{"text":"existing user text","type":"user"}"#);
    insert_message(&database, "msg_assistant", "assistant", 2, r#"{"type":"assistant","time":{"completed":2}}"#);
    insert_text_part(&database, "msg_assistant", "existing answer");

    ensure(&database);
    drain(&database);

    assert_eq!(count(&database, "SELECT count(*) FROM session_search_document"), 3);
    let state = search_index::state(&database).expect("read the search state").expect("the search state");
    assert_eq!(state.total, 3);
    assert_eq!(state.processed, 3);

    let before = schema(&database);
    let state = ensure(&database);
    assert_eq!(state.status, Status::Done);
    let after = search_index::state(&database).expect("read the search state").expect("the search state");
    assert_eq!(after.processed, 3);
    assert_eq!(after.total, 3);
    assert_eq!(after.status, Status::Done);
    assert_eq!(schema(&database), before);
}
