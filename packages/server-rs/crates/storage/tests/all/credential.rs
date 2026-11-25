use opencode_storage::credential::{self, NewCredential};
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

fn seed(connection: &rusqlite::Connection, id: &str, integration_id: Option<&str>, active: i64, time_created: i64) {
    connection
        .execute(
            "INSERT INTO credential (id, integration_id, label, value, active, time_created, time_updated)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            rusqlite::params![id, integration_id, format!("label {id}"), format!("value {id}"), active, time_created],
        )
        .expect("seed a credential");
}

#[test]
fn insert_round_trips_every_field_and_leaves_connector_columns_null() {
    let (_dir, database) = database();
    let active = NewCredential { id: "c1", integration_id: "i1", label: "Active", value: "secret-a", active: true };
    let inactive =
        NewCredential { id: "c2", integration_id: "i1", label: "Inactive", value: "secret-b", active: false };
    read(&database, |connection| credential::insert(connection, &active));
    read(&database, |connection| credential::insert(connection, &inactive));

    let stored = read(&database, |connection| credential::get(connection, "c1")).expect("the credential is present");
    assert_eq!(stored.id, "c1");
    assert_eq!(stored.integration_id.as_deref(), Some("i1"));
    assert_eq!(stored.label, "Active");
    assert_eq!(stored.value, "secret-a");
    assert_eq!(stored.active, Some(true));
    assert!(stored.time_created > 0, "the insert stamps a real time_created");
    assert_eq!(stored.time_created, stored.time_updated, "the insert stamps the same time_updated");

    let rows = read(&database, |connection| credential::list(connection, "i1"));
    assert_eq!(rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), vec!["c2", "c1"]);
    assert_eq!(rows[0].active, Some(false));
    assert_eq!(rows[1].active, Some(true));

    let all = read(&database, credential::list_all);
    assert_eq!(all, rows);

    let columns = read(&database, |connection| {
        Ok(connection.query_row("SELECT connector_id, method_id FROM credential WHERE id = 'c1'", [], |row| {
            Ok((row.get::<_, Option<String>>(0)?, row.get::<_, Option<String>>(1)?))
        })?)
    });
    assert_eq!(columns, (None, None), "connector_id and method_id stay NULL");
}

#[test]
fn list_all_skips_rows_without_an_integration_and_orders_active_then_time_then_id() {
    let (_dir, database) = database();
    read(&database, |connection| {
        seed(connection, "orphan", None, 0, 1);
        seed(connection, "b", Some("i1"), 0, 1);
        seed(connection, "c", Some("i2"), 0, 1);
        seed(connection, "z", Some("i1"), 0, 1);
        seed(connection, "a", Some("i1"), 0, 5);
        seed(connection, "d", Some("i2"), 1, 1);
        Ok(())
    });

    let all = read(&database, credential::list_all);
    assert_eq!(
        all.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        vec!["b", "c", "z", "a", "d"],
        "rows sort by active, then time_created, then id and skip a NULL integration_id"
    );

    let filtered = read(&database, |connection| credential::list(connection, "i1"));
    assert_eq!(
        filtered.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        vec!["b", "z", "a"],
        "list uses the same ordering"
    );
}

#[test]
fn list_filters_to_one_integration() {
    let (_dir, database) = database();
    read(&database, |connection| {
        seed(connection, "a", Some("i1"), 0, 1);
        seed(connection, "b", Some("i1"), 0, 2);
        seed(connection, "c", Some("i2"), 0, 1);
        Ok(())
    });

    let rows = read(&database, |connection| credential::list(connection, "i1"));
    assert_eq!(rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), vec!["a", "b"]);
    assert!(rows.iter().all(|row| row.integration_id.as_deref() == Some("i1")));
}

#[test]
fn set_active_clears_siblings_of_one_integration_only() {
    let (_dir, database) = database();
    read(&database, |connection| {
        seed(connection, "a", Some("i1"), 1, 1);
        seed(connection, "b", Some("i1"), 0, 2);
        seed(connection, "c", Some("i1"), 0, 3);
        seed(connection, "x", Some("i2"), 1, 1);
        seed(connection, "y", Some("i2"), 0, 2);
        Ok(())
    });

    read(&database, |connection| credential::set_active(connection, "i1", "b"));

    let i1 = read(&database, |connection| credential::list(connection, "i1"));
    assert_eq!(
        i1.iter().map(|row| (row.id.as_str(), row.active)).collect::<Vec<_>>(),
        vec![("a", Some(false)), ("c", Some(false)), ("b", Some(true))]
    );

    let i2 = read(&database, |connection| credential::list(connection, "i2"));
    assert_eq!(
        i2.iter().map(|row| (row.id.as_str(), row.active)).collect::<Vec<_>>(),
        vec![("y", Some(false)), ("x", Some(true))],
        "another integration is untouched"
    );
}

#[test]
fn pin_active_sets_one_row_without_clearing_siblings() {
    let (_dir, database) = database();
    read(&database, |connection| {
        seed(connection, "a", Some("i1"), 0, 1);
        seed(connection, "b", Some("i1"), 1, 2);
        seed(connection, "c", Some("i1"), 0, 3);
        Ok(())
    });

    read(&database, |connection| credential::pin_active(connection, "a"));

    let rows = read(&database, |connection| credential::list(connection, "i1"));
    assert_eq!(
        rows.iter().map(|row| (row.id.as_str(), row.active)).collect::<Vec<_>>(),
        vec![("c", Some(false)), ("a", Some(true)), ("b", Some(true))]
    );
}

#[test]
fn update_label_and_update_value_write_and_bump_time_updated() {
    let (_dir, database) = database();
    let credential_row =
        NewCredential { id: "c1", integration_id: "i1", label: "Old", value: "old-value", active: true };
    read(&database, |connection| credential::insert(connection, &credential_row));
    read(&database, |connection| {
        connection.execute("UPDATE credential SET time_updated = 1 WHERE id = 'c1'", [])?;
        Ok(())
    });

    assert!(read(&database, |connection| credential::update_label(connection, "c1", "New")));
    let stored = read(&database, |connection| credential::get(connection, "c1")).expect("the credential is present");
    assert_eq!(stored.label, "New");
    assert!(stored.time_updated > 1, "update_label bumps time_updated");

    read(&database, |connection| {
        connection.execute("UPDATE credential SET time_updated = 1 WHERE id = 'c1'", [])?;
        Ok(())
    });
    assert!(read(&database, |connection| credential::update_value(connection, "c1", "new-value")));
    let stored = read(&database, |connection| credential::get(connection, "c1")).expect("the credential is present");
    assert_eq!(stored.value, "new-value");
    assert!(stored.time_updated > 1, "update_value bumps time_updated");
}

#[test]
fn update_label_and_update_value_report_an_unknown_id() {
    let (_dir, database) = database();

    assert!(!read(&database, |connection| credential::update_label(connection, "missing", "New")));
    assert!(!read(&database, |connection| credential::update_value(connection, "missing", "new-value")));
}

#[test]
fn delete_removes_the_row() {
    let (_dir, database) = database();
    let credential_row = NewCredential { id: "c1", integration_id: "i1", label: "Label", value: "value", active: true };
    read(&database, |connection| credential::insert(connection, &credential_row));

    read(&database, |connection| credential::delete(connection, "c1"));

    assert_eq!(read(&database, |connection| credential::get(connection, "c1")), None);
    assert!(read(&database, credential::list_all).is_empty());
}
