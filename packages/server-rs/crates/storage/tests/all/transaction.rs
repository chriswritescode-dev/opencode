//! `Database::transaction`: commit, rollback, savepoints and a failed commit.

use opencode_storage::database::open;
use opencode_storage::{Database, DatabaseLocation, StorageError};
use rusqlite::TransactionBehavior;

use crate::TempDir;

fn database() -> (TempDir, Database) {
    let dir = TempDir::new();
    let (database, _) = open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    (dir, database)
}

fn create_table(database: &Database, sql: &str) {
    database
        .read(|connection| {
            connection.execute_batch(sql)?;
            Ok(())
        })
        .expect("create the tables");
}

fn count(database: &Database, table: &str) -> i64 {
    database
        .read(|connection| Ok(connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))?))
        .expect("count the rows")
}

fn ids(database: &Database) -> Vec<i64> {
    database
        .read(|connection| {
            let mut statement = connection.prepare("SELECT id FROM t ORDER BY id")?;
            let ids = statement.query_map([], |row| row.get(0))?.collect::<Result<Vec<i64>, _>>()?;
            Ok(ids)
        })
        .expect("read the ids")
}

#[test]
fn commit_keeps_the_rows() {
    let (_dir, database) = database();
    create_table(&database, "CREATE TABLE t (id INTEGER)");
    database
        .transaction(TransactionBehavior::Immediate, |tx| {
            tx.execute("INSERT INTO t (id) VALUES (1)", [])?;
            Ok::<(), StorageError>(())
        })
        .expect("commit the insert");
    assert_eq!(count(&database, "t"), 1, "the committed row is kept");
}

#[test]
fn an_error_rolls_back_every_write() {
    let (_dir, database) = database();
    create_table(&database, "CREATE TABLE t (id INTEGER)");
    let result: Result<(), StorageError> = database.transaction(TransactionBehavior::Immediate, |tx| {
        tx.execute("INSERT INTO t (id) VALUES (1)", [])?;
        tx.execute("INSERT INTO t (id) VALUES (2)", [])?;
        Err(StorageError::Migration("stop".into()))
    });
    let error = result.expect_err("the transaction reports the error from f");
    assert_eq!(error.to_string(), "stop", "the migration message is returned verbatim");
    assert_eq!(count(&database, "t"), 0, "both inserts are rolled back");
}

#[test]
fn a_rolled_back_savepoint_keeps_the_outer_writes() {
    let (_dir, database) = database();
    create_table(&database, "CREATE TABLE t (id INTEGER)");
    database
        .transaction(TransactionBehavior::Immediate, |tx| {
            tx.execute("INSERT INTO t (id) VALUES (1)", [])?;
            {
                let mut savepoint = tx.savepoint()?;
                savepoint.execute("INSERT INTO t (id) VALUES (2)", [])?;
                savepoint.rollback()?;
            }
            tx.execute("INSERT INTO t (id) VALUES (3)", [])?;
            Ok::<(), StorageError>(())
        })
        .expect("commit the outer transaction");
    assert_eq!(ids(&database), vec![1, 3], "only the savepoint's insert is undone");
}

#[test]
fn a_deferred_foreign_key_violation_fails_the_commit() {
    let (_dir, database) = database();
    create_table(
        &database,
        "CREATE TABLE parent (id INTEGER PRIMARY KEY);
         CREATE TABLE child (parent_id INTEGER REFERENCES parent(id));",
    );
    let result: Result<(), StorageError> = database.transaction(TransactionBehavior::Immediate, |tx| {
        tx.execute_batch("PRAGMA defer_foreign_keys = ON")?;
        tx.execute("INSERT INTO child (parent_id) VALUES (1)", [])?;
        Ok(())
    });
    assert!(matches!(result, Err(StorageError::Sqlite(_))), "the commit fails: {result:?}");
    assert!(
        database.read(|connection| Ok(connection.is_autocommit())).expect("read the autocommit flag"),
        "the connection is usable after the failed commit"
    );
    assert_eq!(count(&database, "child"), 0, "the violating insert is rolled back");
}
