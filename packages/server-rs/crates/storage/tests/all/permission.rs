use opencode_storage::permission::{self, UpdateOutcome};
use opencode_storage::project;
use opencode_storage::{Database, DatabaseLocation, StorageError};
use rusqlite::params;

use crate::TempDir;

fn database() -> (TempDir, Database) {
    let dir = TempDir::new();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    (dir, database)
}

fn read<T>(database: &Database, f: impl FnOnce(&rusqlite::Connection) -> Result<T, StorageError>) -> T {
    database.read(f).expect("read the database")
}

fn count(database: &Database, sql: &str) -> i64 {
    read(database, |connection| Ok(connection.query_row(sql, [], |row| row.get(0))?))
}

fn project(database: &Database, id: &str) {
    read(database, |connection| project::upsert(connection, id, &format!("/work/{id}"), None));
}

#[test]
fn add_without_resources_writes_nothing() {
    let (_dir, database) = database();
    project(&database, "p1");

    read(&database, |connection| permission::add(connection, "p1", "read", &[]));

    assert_eq!(count(&database, "SELECT COUNT(*) FROM permission"), 0);
}

#[test]
fn add_inserts_one_row_per_resource_and_ignores_a_repeated_add() {
    let (_dir, database) = database();
    project(&database, "p1");

    read(&database, |connection| permission::add(connection, "p1", "read", &["a".into(), "b".into()]));

    let rows = read(&database, |connection| permission::list(connection, Some("p1")));
    assert_eq!(rows.len(), 2, "each resource gets a row");
    assert!(rows.iter().all(|row| row.id.starts_with("psv_")), "the rows use the permission id prefix");
    assert_eq!(rows.iter().map(|row| row.resource.as_str()).collect::<Vec<_>>(), ["a", "b"]);
    assert!(rows.iter().all(|row| row.action == "read" && row.project_id == "p1"));
    assert!(rows.iter().all(|row| row.time_created == row.time_updated && row.time_updated != 0));

    read(&database, |connection| permission::add(connection, "p1", "read", &["a".into(), "b".into()]));

    assert_eq!(count(&database, "SELECT COUNT(*) FROM permission"), 2, "a repeated add keeps one row per resource");
}

#[test]
fn list_sees_every_project_and_filters_by_project_in_insertion_order() {
    let (_dir, database) = database();
    project(&database, "p1");
    project(&database, "p2");
    read(&database, |connection| permission::add(connection, "p1", "read", &["a".into(), "b".into()]));
    read(&database, |connection| permission::add(connection, "p2", "edit", &["c".into()]));

    let all = read(&database, |connection| permission::list(connection, None));
    assert_eq!(all.iter().map(|row| row.resource.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);

    let filtered = read(&database, |connection| permission::list(connection, Some("p1")));
    assert_eq!(filtered.iter().map(|row| row.resource.as_str()).collect::<Vec<_>>(), ["a", "b"]);
}

#[test]
fn update_overrides_one_field_and_bumps_time_updated() {
    let (_dir, database) = database();
    project(&database, "p1");
    read(&database, |connection| permission::add(connection, "p1", "read", &["a".into()]));
    let before = read(&database, |connection| permission::list(connection, Some("p1"))).remove(0);
    read(&database, |connection| {
        connection.execute("UPDATE permission SET time_updated = 1 WHERE id = ?1", params![before.id])?;
        Ok(())
    });

    let outcome = read(&database, |connection| permission::update(connection, &before.id, Some("write"), None));

    let UpdateOutcome::Updated(updated) = outcome else {
        panic!("the update succeeds");
    };
    assert_eq!(updated.action, "write", "the provided override is stored");
    assert_eq!(updated.resource, before.resource, "the other field keeps its value");
    assert_eq!(updated.time_created, before.time_created);
    assert!(updated.time_updated > 1, "the update bumps time_updated");
    let stored = read(&database, |connection| permission::get(connection, &before.id)).expect("the row is present");
    assert_eq!(stored, updated);
}

#[test]
fn update_to_a_saved_resource_conflicts_and_leaves_the_row_unchanged() {
    let (_dir, database) = database();
    project(&database, "p1");
    read(&database, |connection| permission::add(connection, "p1", "read", &["a".into(), "b".into()]));
    let rows = read(&database, |connection| permission::list(connection, Some("p1")));
    let first = rows[0].clone();
    let second = rows[1].clone();

    let outcome =
        read(&database, |connection| permission::update(connection, &first.id, None, Some(second.resource.as_str())));

    assert_eq!(outcome, UpdateOutcome::Conflict);
    let stored = read(&database, |connection| permission::get(connection, &first.id)).expect("the row is present");
    assert_eq!(stored, first, "a conflict leaves the row unchanged");
}

#[test]
fn update_of_a_missing_id_is_not_found() {
    let (_dir, database) = database();

    let outcome = read(&database, |connection| permission::update(connection, "psv_missing", Some("read"), None));

    assert_eq!(outcome, UpdateOutcome::NotFound);
}

#[test]
fn update_that_changes_no_row_is_not_found() {
    let (_dir, database) = database();
    project(&database, "p1");
    read(&database, |connection| permission::add(connection, "p1", "read", &["a".into()]));
    let row = read(&database, |connection| permission::list(connection, Some("p1"))).remove(0);
    read(&database, |connection| {
        connection.execute_batch(
            "CREATE TRIGGER permission_update_ignored BEFORE UPDATE ON permission BEGIN SELECT RAISE(IGNORE); END",
        )?;
        Ok(())
    });

    let outcome = read(&database, |connection| permission::update(connection, &row.id, Some("write"), None));

    assert_eq!(outcome, UpdateOutcome::NotFound, "a write that no longer matches a row is not reported as updated");
}

#[test]
fn remove_deletes_the_row_and_a_second_remove_is_harmless() {
    let (_dir, database) = database();
    project(&database, "p1");
    read(&database, |connection| permission::add(connection, "p1", "read", &["a".into()]));
    let row = read(&database, |connection| permission::list(connection, None)).remove(0);

    read(&database, |connection| permission::remove(connection, &row.id));

    assert_eq!(read(&database, |connection| permission::get(connection, &row.id)), None);
    read(&database, |connection| permission::remove(connection, &row.id));
    assert_eq!(count(&database, "SELECT COUNT(*) FROM permission"), 0);
}
