use opencode_storage::project;
use opencode_storage::worktree;
use opencode_storage::{Database, DatabaseLocation, StorageError};

use crate::TempDir;

fn database() -> (TempDir, Database) {
    let dir = TempDir::new();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    (dir, database)
}

fn read<T>(database: &Database, f: impl FnOnce(&rusqlite::Connection) -> Result<T, StorageError>) -> T {
    database.read(f).expect("read the database")
}

fn project(database: &Database, id: &str) {
    read(database, |connection| project::upsert(connection, id, "/work/project", None));
}

#[test]
fn upsert_inserts_a_row_and_reports_the_change() {
    let (_dir, database) = database();
    project(&database, "p1");

    let inserted = read(&database, |connection| worktree::upsert(connection, "p1", "/work/project", None, true));

    assert!(inserted, "a missing row is inserted");
    let stored = read(&database, |connection| worktree::find(connection, "p1", "/work/project"))
        .expect("the worktree is present");
    assert_eq!(stored.project_id, "p1");
    assert_eq!(stored.directory, "/work/project");
    assert_eq!(stored.strategy, None);
    assert_ne!(stored.time_created, 0, "the insert stamps time_created");
}

#[test]
fn list_orders_by_time_created_descending_then_directory() {
    let (_dir, database) = database();
    project(&database, "p1");
    for (directory, time) in [("/work/b", 1), ("/work/a", 1), ("/work/c", 2)] {
        read(&database, |connection| {
            connection.execute(
                "INSERT INTO worktree (project_id, directory, strategy, time_created) VALUES ('p1', ?1, NULL, ?2)",
                rusqlite::params![directory, time],
            )?;
            Ok(())
        });
    }

    let rows = read(&database, |connection| worktree::list(connection, "p1"));

    assert_eq!(rows.iter().map(|row| row.directory.as_str()).collect::<Vec<_>>(), ["/work/c", "/work/a", "/work/b"]);
}

#[test]
fn list_only_returns_the_requested_project() {
    let (_dir, database) = database();
    project(&database, "p1");
    project(&database, "p2");
    read(&database, |connection| worktree::insert_ignore(connection, "p1", "/work/one", None));
    read(&database, |connection| worktree::insert_ignore(connection, "p2", "/work/two", None));

    let rows = read(&database, |connection| worktree::list(connection, "p1"));

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].directory, "/work/one");
}

#[test]
fn insert_ignore_never_replaces_an_owned_row() {
    let (_dir, database) = database();
    project(&database, "p1");
    read(&database, |connection| worktree::upsert(connection, "p1", "/work/project", Some("git"), true));

    read(&database, |connection| worktree::insert_ignore(connection, "p1", "/work/project", None));

    let stored = read(&database, |connection| worktree::find(connection, "p1", "/work/project"))
        .expect("the worktree is present");
    assert_eq!(stored.strategy.as_deref(), Some("git"), "the existing strategy is kept");
}

#[test]
fn upsert_without_replace_claims_only_unowned_rows() {
    let (_dir, database) = database();
    project(&database, "p1");
    read(&database, |connection| worktree::upsert(connection, "p1", "/work/project", None, true));

    let claimed = read(&database, |connection| worktree::upsert(connection, "p1", "/work/project", Some("git"), false));
    assert!(claimed, "discovery claims an unowned row");
    let stored = read(&database, |connection| worktree::find(connection, "p1", "/work/project"))
        .expect("the worktree is present");
    assert_eq!(stored.strategy.as_deref(), Some("git"));

    let replaced =
        read(&database, |connection| worktree::upsert(connection, "p1", "/work/project", Some("other"), false));
    assert!(!replaced, "another strategy never replaces an owned row");
    let stored = read(&database, |connection| worktree::find(connection, "p1", "/work/project"))
        .expect("the worktree is present");
    assert_eq!(stored.strategy.as_deref(), Some("git"), "the original strategy is kept");

    let unowned = read(&database, |connection| worktree::upsert(connection, "p1", "/work/project", None, false));
    assert!(!unowned, "a missing strategy never settles an owned row");
}

#[test]
fn upsert_with_replace_overwrites_the_strategy() {
    let (_dir, database) = database();
    project(&database, "p1");
    read(&database, |connection| worktree::upsert(connection, "p1", "/work/project", Some("git"), true));

    let replaced =
        read(&database, |connection| worktree::upsert(connection, "p1", "/work/project", Some("custom"), true));

    assert!(replaced);
    let stored = read(&database, |connection| worktree::find(connection, "p1", "/work/project"))
        .expect("the worktree is present");
    assert_eq!(stored.strategy.as_deref(), Some("custom"));
}

#[test]
fn delete_reports_whether_a_row_was_removed() {
    let (_dir, database) = database();
    project(&database, "p1");
    read(&database, |connection| worktree::insert_ignore(connection, "p1", "/work/project", None));

    assert!(read(&database, |connection| worktree::delete(connection, "p1", "/work/project")));
    assert!(!read(&database, |connection| worktree::delete(connection, "p1", "/work/project")));
    assert_eq!(read(&database, |connection| worktree::find(connection, "p1", "/work/project")), None);
}

#[test]
fn upsert_rejects_a_relative_directory() {
    let (_dir, database) = database();
    project(&database, "p1");

    let error = database
        .read(|connection| worktree::upsert(connection, "p1", "relative/path", None, true))
        .expect_err("a relative directory is not stored");

    assert_eq!(error.to_string(), "Path is not absolute: relative/path");
}
