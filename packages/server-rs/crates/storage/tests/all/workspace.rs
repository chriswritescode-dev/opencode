use opencode_storage::workspace;
use opencode_storage::{Database, DatabaseLocation, StorageError};
use serde_json::json;

use crate::TempDir;

fn database() -> (TempDir, Database) {
    let dir = TempDir::new();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    (dir, database)
}

fn read<T>(database: &Database, f: impl FnOnce(&rusqlite::Connection) -> Result<T, StorageError>) -> T {
    database.read(f).expect("read the database")
}

#[test]
fn insert_round_trips_through_get() {
    let (_dir, database) = database();
    let binding = json!({ "key": "value" });

    let inserted = read(&database, |connection| workspace::insert(connection, "w1", "local", Some(&binding)));

    let stored = read(&database, |connection| workspace::get(connection, "w1")).expect("the workspace is present");
    assert_eq!(stored, inserted);
    assert_eq!(stored.id, "w1");
    assert_eq!(stored.provider, "local");
    assert_eq!(stored.binding, Some(json!({ "key": "value" })));
    assert_ne!(stored.created_at, 0, "the insert stamps created_at");
    assert_ne!(stored.last_used_at, 0, "the insert stamps last_used_at");
}

#[test]
fn insert_without_a_binding_stores_null() {
    let (_dir, database) = database();

    let inserted = read(&database, |connection| workspace::insert(connection, "w1", "local", None));

    assert_eq!(inserted.binding, None);
    let stored = read(&database, |connection| workspace::get(connection, "w1")).expect("the workspace is present");
    assert_eq!(stored.binding, None, "a missing binding stays SQL NULL");
}

#[test]
fn list_returns_rows_in_insertion_order() {
    let (_dir, database) = database();
    for id in ["w1", "w2", "w3"] {
        read(&database, |connection| workspace::insert(connection, id, "local", None));
    }

    let rows = read(&database, workspace::list);

    assert_eq!(rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), ["w1", "w2", "w3"]);
}

#[test]
fn touch_bumps_last_used_at() {
    let (_dir, database) = database();
    read(&database, |connection| workspace::insert(connection, "w1", "local", None));
    read(&database, |connection| {
        connection.execute("UPDATE workspace SET last_used_at = 1 WHERE id = 'w1'", [])?;
        Ok(())
    });

    read(&database, |connection| workspace::touch(connection, "w1"));

    let stored = read(&database, |connection| workspace::get(connection, "w1")).expect("the workspace is present");
    assert!(stored.last_used_at > 1, "touch bumps last_used_at");
}

#[test]
fn delete_reports_whether_a_row_was_removed() {
    let (_dir, database) = database();
    read(&database, |connection| workspace::insert(connection, "w1", "local", None));

    assert!(read(&database, |connection| workspace::delete(connection, "w1")), "the first delete removes the row");
    assert!(!read(&database, |connection| workspace::delete(connection, "w1")), "the second delete finds nothing");
    assert_eq!(read(&database, |connection| workspace::get(connection, "w1")), None, "the deleted row is gone");
}
