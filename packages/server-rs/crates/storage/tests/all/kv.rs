//! The `kv` table: reading and writing values, and scanning keys.

use opencode_storage::kv::{self, ScanOptions, ScanResult};
use opencode_storage::{Database, DatabaseLocation, StorageError, database::open};
use rusqlite::TransactionBehavior;
use serde_json::{Value, json};

use crate::TempDir;

fn database() -> (TempDir, Database) {
    let dir = TempDir::new();
    let (database, _) = open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    (dir, database)
}

fn insert(database: &Database, keys: impl IntoIterator<Item = String>) {
    database
        .transaction(TransactionBehavior::Immediate, |transaction| {
            let mut statement =
                transaction.prepare("INSERT INTO kv (key, value, time_created, time_updated) VALUES (?, '0', 0, 0)")?;
            for key in keys {
                statement.execute([key])?;
            }
            Ok::<(), StorageError>(())
        })
        .expect("insert the keys");
}

fn scan(database: &Database, options: ScanOptions) -> ScanResult {
    database.read(|connection| kv::scan(connection, options)).expect("scan the keys")
}

fn keys(result: &ScanResult) -> Vec<&str> {
    result.entries.iter().map(|entry| entry.key.as_str()).collect()
}

fn stored(database: &Database, key: &str) -> (String, i64, i64) {
    database
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT value, time_created, time_updated FROM kv WHERE key = ?",
                [key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?)
        })
        .expect("read the stored row")
}

#[test]
fn set_and_get_round_trip_json_values() {
    let (_dir, database) = database();
    database
        .read(|connection| {
            for (key, value) in [
                ("object", json!({ "a": 1, "b": [true, null] })),
                ("array", json!([1, "two", false])),
                ("number", json!(1.5)),
            ] {
                kv::set(connection, key, &value)?;
                assert_eq!(kv::get(connection, key)?, Some(value), "key {key}");
            }
            assert_eq!(kv::get(connection, "missing")?, None, "a missing key is absent");
            Ok(())
        })
        .expect("round trip the values");
}

#[test]
fn a_whole_number_is_stored_as_one() {
    let (_dir, database) = database();
    database
        .read(|connection| {
            kv::set(connection, "number", &json!(1.0))?;
            Ok(())
        })
        .expect("store the number");
    assert_eq!(stored(&database, "number").0, "1", "the stored text is the JavaScript text");
    assert_eq!(
        database.read(|connection| kv::get(connection, "number")).expect("read the number"),
        Some(json!(1)),
        "the value reads back as a whole number"
    );
}

#[test]
fn setting_again_keeps_time_created_and_moves_time_updated() {
    let (_dir, database) = database();
    database
        .read(|connection| {
            kv::set(connection, "k", &json!({ "v": 1 }))?;
            connection.execute("UPDATE kv SET time_created = 111, time_updated = 222 WHERE key = ?", ["k"])?;
            kv::set(connection, "k", &json!({ "v": 2 }))?;
            Ok(())
        })
        .expect("set the key twice");
    let (value, created, updated) = stored(&database, "k");
    assert_eq!(created, 111, "time_created is kept");
    assert_ne!(updated, 222, "time_updated is refreshed");
    assert!(updated > 222, "time_updated moves forward");
    assert_eq!(
        serde_json::from_str::<Value>(&value).expect("parse the stored value"),
        json!({ "v": 2 }),
        "the value is replaced"
    );
}

#[test]
fn remove_deletes_a_key_and_a_missing_key_is_not_an_error() {
    let (_dir, database) = database();
    database
        .read(|connection| {
            kv::set(connection, "k", &json!("value"))?;
            kv::remove(connection, "k")?;
            assert_eq!(kv::get(connection, "k")?, None, "the removed key is absent");
            kv::remove(connection, "missing")?;
            Ok(())
        })
        .expect("remove the keys");
}

#[test]
fn scan_returns_the_prefixed_keys_in_ascending_order() {
    let (_dir, database) = database();
    insert(&database, ["b", "a2", "a1", "a3", "c"].map(String::from));
    let result = scan(&database, ScanOptions { prefix: "a".into(), after: None, limit: None });
    assert_eq!(keys(&result), ["a1", "a2", "a3"], "only the prefixed keys are returned, in order");
    assert_eq!(result.next, None, "every matching row is returned");
}

#[test]
fn scan_after_excludes_the_keys_up_to_and_including_it() {
    let (_dir, database) = database();
    insert(&database, ["a", "b", "c", "d"].map(String::from));
    let result = scan(&database, ScanOptions { prefix: "".into(), after: Some("b".into()), limit: None });
    assert_eq!(keys(&result), ["c", "d"], "keys at or below `after` are excluded");
}

#[test]
fn limit_is_clamped_between_one_and_one_thousand() {
    let (_dir, database) = database();
    insert(&database, ["small/a", "small/b", "small/c"].map(String::from));
    let result = scan(&database, ScanOptions { prefix: "small/".into(), after: None, limit: Some(0.0) });
    assert_eq!(keys(&result), ["small/a"], "a limit below one returns one entry");
    assert_eq!(result.next.as_deref(), Some("small/a"), "the truncated scan continues after its entry");

    insert(&database, (0..1001).map(|index| format!("many/{index:04}")));
    let result = scan(&database, ScanOptions { prefix: "many/".into(), after: None, limit: Some(5000.0) });
    assert_eq!(result.entries.len(), 1000, "a limit above one thousand returns one thousand entries");
    assert_eq!(result.next.as_deref(), Some("many/0999"), "the next key is the last returned one");

    insert(&database, (0..101).map(|index| format!("nan/{index:04}")));
    let result = scan(&database, ScanOptions { prefix: "nan/".into(), after: None, limit: Some(f64::NAN) });
    assert_eq!(result.entries.len(), 100, "a NaN limit returns the default of one hundred entries");
    assert_eq!(result.next.as_deref(), Some("nan/0099"), "the next key is the last returned one");
}

#[test]
fn scan_reports_no_next_when_every_row_fits_and_an_empty_prefix_scans_everything() {
    let (_dir, database) = database();
    insert(&database, ["a", "b"].map(String::from));
    let result = scan(&database, ScanOptions { prefix: "".into(), after: None, limit: None });
    assert_eq!(keys(&result), ["a", "b"], "an empty prefix scopes no key");
    assert_eq!(result.next, None, "a scan with no extra row reports no next key");
}

#[test]
fn a_prefix_ending_in_the_maximum_code_point_scans_its_range() {
    let (_dir, database) = database();
    insert(&database, ["a\u{10FFFF}", "a\u{10FFFF}b", "b"].map(String::from));
    let result = scan(&database, ScanOptions { prefix: "a\u{10FFFF}".into(), after: None, limit: None });
    assert_eq!(keys(&result), ["a\u{10FFFF}", "a\u{10FFFF}b"], "only the keys at or above the prefix are returned");
}

#[test]
fn a_prefix_of_only_the_maximum_code_point_has_no_end() {
    let (_dir, database) = database();
    insert(&database, ["\u{10FFFF}", "\u{10FFFF}a", "\u{10FFFE}", "z"].map(String::from));
    let result = scan(&database, ScanOptions { prefix: "\u{10FFFF}".into(), after: None, limit: None });
    assert_eq!(keys(&result), ["\u{10FFFF}", "\u{10FFFF}a"], "the lower bound applies and no upper bound is added");
    assert_eq!(result.next, None, "every matching row is returned");
}

#[test]
fn a_lone_surrogate_in_the_prefix_becomes_the_replacement_character() {
    let (_dir, database) = database();
    insert(
        &database,
        ["x\u{D7FF}\u{FFFD}", "x\u{D7FF}\u{FFFF}", "x\u{F000}", "x\u{FFFD}", "x\u{FFFE}"].map(String::from),
    );
    let result = scan(&database, ScanOptions { prefix: "x\u{D7FF}".into(), after: None, limit: None });
    assert_eq!(
        keys(&result),
        ["x\u{D7FF}\u{FFFD}", "x\u{D7FF}\u{FFFF}", "x\u{F000}"],
        "the end is the replacement character, so keys below it are returned"
    );
}
