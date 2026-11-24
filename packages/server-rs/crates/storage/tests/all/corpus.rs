use std::path::Path;

use opencode_storage::dump::{Masks, dump};
use opencode_storage::{Database, DatabaseLocation};
use serde_json::Value;

use crate::TempDir;

const CORPUS: &str = include_str!("../corpus/migrations.json");

#[test]
fn corpus_matches_the_pinned_typescript() {
    let _guard = crate::lock_identifiers();
    let corpus: Value = serde_json::from_str(CORPUS).expect("parse the migration corpus");
    let fixtures = corpus["fixtures"].as_array().expect("the corpus has a fixtures array");
    assert_eq!(fixtures.len(), 14, "the corpus holds every pinned fixture");
    let masks: Masks = serde_json::from_value(corpus["masks"].clone()).expect("parse the masks");

    for fixture in fixtures {
        let name = fixture["name"].as_str().expect("a fixture name");
        let dir = TempDir::new();
        if let Some(auth_json) = fixture["authJson"].as_str() {
            std::fs::write(dir.path().join("auth.json"), auth_json).expect("write the legacy credential file");
        }
        let path = dir.path().join("opencode.db");
        seed(&path, fixture["start"].as_array().expect("a start array"), name);

        let expected = &fixture["expected"];
        let result = Database::open(DatabaseLocation::File(path.clone()), dir.path());
        match result {
            Ok((database, _)) => {
                assert!(!expected.is_null(), "fixture {name} was expected to fail");
                let actual = database.read(|connection| dump(connection, &masks)).expect("dump the database");
                assert_dump(name, expected, &actual);
                drop(database);

                let (database, report) =
                    Database::open(DatabaseLocation::File(path.clone()), dir.path()).expect("reopen the database");
                assert!(!report.bootstrapped, "fixture {name}: the second open does not bootstrap");
                assert!(report.applied.is_empty(), "fixture {name}: the second open applies nothing");
                let actual = database.read(|connection| dump(connection, &masks)).expect("dump the database again");
                assert_dump(name, expected, &actual);
            }
            Err(error) => {
                assert!(expected.is_null(), "fixture {name} failed with {error} but has an expected dump");
                let Some(expected_error) = fixture["error"].as_str() else {
                    panic!("fixture {name} was expected to succeed, but failed with: {error}");
                };
                assert_eq!(error.to_string(), expected_error, "fixture {name}");
            }
        }
    }
}

fn seed(path: &Path, statements: &[Value], name: &str) {
    let connection = rusqlite::Connection::open(path).expect("open a raw connection");
    connection.execute_batch("PRAGMA foreign_keys = OFF").expect("disable foreign keys for the seed");
    for statement in statements {
        let statement = statement.as_str().expect("a start statement");
        connection
            .execute(statement, [])
            .unwrap_or_else(|error| panic!("fixture {name} failed to run {statement}: {error}"));
    }
}

fn assert_dump(name: &str, expected: &Value, actual: &Value) {
    assert_eq!(actual["schema"], expected["schema"], "fixture {name}: the schema");
    assert_eq!(
        actual["foreignKeyViolations"], expected["foreignKeyViolations"],
        "fixture {name}: the foreign key violations"
    );
    let expected_tables = expected["tables"].as_object().expect("the expected tables");
    let actual_tables = actual["tables"].as_object().expect("the actual tables");
    let mut names: Vec<&String> = expected_tables.keys().chain(actual_tables.keys()).collect();
    names.sort();
    names.dedup();
    for table in names {
        let (Some(expected_table), Some(actual_table)) = (expected_tables.get(table), actual_tables.get(table)) else {
            panic!("fixture {name}: table {table} is missing on one side");
        };
        assert_eq!(actual_table["columns"], expected_table["columns"], "fixture {name}: table {table} columns");
        let expected_rows = sorted_rows(&expected_table["rows"]);
        let actual_rows = sorted_rows(&actual_table["rows"]);
        assert_eq!(
            actual_rows,
            expected_rows,
            "fixture {name}: table {table} rows\n expected: {}\n   actual: {}",
            serde_json::to_string(&expected_rows).expect("serialize the expected rows"),
            serde_json::to_string(&actual_rows).expect("serialize the actual rows")
        );
    }
}

fn sorted_rows(rows: &Value) -> Vec<Value> {
    let mut rows = rows.as_array().expect("the table rows").clone();
    rows.sort_by_key(|row| serde_json::to_string(row).expect("serialize a row"));
    rows
}
