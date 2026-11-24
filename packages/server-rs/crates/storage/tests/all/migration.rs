use std::path::Path;

use opencode_storage::generated::migrations::{Body, MIGRATIONS, Migration};
use opencode_storage::migration::hand::HAND_PORTED;
use opencode_storage::migration::{apply_only, completed};
use opencode_storage::{Database, DatabaseLocation};

use crate::TempDir;

const LEGACY_CREDENTIALS: &str = r#"{"openai":{"type":"oauth","refresh":"refresh","access":"access","expires":123,"accountId":"account"},"anthropic":{"type":"api","key":"legacy-key","metadata":{"region":"us"}},"google":{"type":"api","key":"google-key","metadata":{"region":"us"}},"github-copilot":{"type":"oauth","refresh":"refresh","access":"access","expires":123},"custom-provider":{"type":"api","key":"custom-key"},"https://example.com/":{"type":"wellknown","key":"TOKEN","token":"wellknown-key"},"invalid":{"type":"unknown"}}"#;

fn file(dir: &TempDir) -> DatabaseLocation {
    DatabaseLocation::File(dir.path().join("opencode.db"))
}

fn seed(path: &Path, sql: &str) {
    let connection = rusqlite::Connection::open(path).expect("open a raw connection");
    connection.execute_batch(sql).expect("seed the database");
}

fn integer(database: &Database, sql: &str) -> i64 {
    database.read(|connection| Ok(connection.query_row(sql, [], |row| row.get(0))?)).expect("read an integer")
}

fn count(database: &Database, sql: &str) -> i64 {
    integer(database, sql)
}

fn exists(database: &Database, sql: &str) -> bool {
    count(database, sql) > 0
}

fn value(database: &Database, sql: &str) -> String {
    database.read(|connection| Ok(connection.query_row(sql, [], |row| row.get(0))?)).expect("read a value")
}

fn completed_ids(database: &Database) -> Vec<String> {
    let mut values = database.read(completed).expect("read the completed migrations");
    values.sort();
    values
}

fn credentials(database: &Database) -> Vec<(String, String, String, String)> {
    database
        .read(|connection| {
            let mut statement = connection
                .prepare("SELECT id, integration_id, label, value FROM credential ORDER BY integration_id")?;
            let rows = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?
                .collect::<Result<Vec<(String, String, String, String)>, _>>()?;
            Ok(rows)
        })
        .expect("read the credentials")
}

fn credential_rows(rows: &[(String, String, String, String)]) -> Vec<(&str, &str, &str)> {
    rows.iter()
        .map(|(_, integration_id, label, value)| (integration_id.as_str(), label.as_str(), value.as_str()))
        .collect()
}

fn assert_credential_id(id: &str) {
    let suffix = id.strip_prefix("cred_").expect("the credential id starts with cred_");
    assert_eq!(suffix.len(), 26, "the credential id suffix is 26 characters: {id}");
    let (timestamp, random) = suffix.split_at(12);
    assert!(
        timestamp.chars().all(|character| character.is_ascii_digit() || ('a'..='f').contains(&character)),
        "the credential id time prefix is lowercase hex: {id}"
    );
    assert!(
        random.chars().all(|character| character.is_ascii_alphanumeric()),
        "the credential id random suffix is alphanumeric: {id}"
    );
}

#[test]
fn bootstraps_an_empty_file() {
    let dir = TempDir::new();
    let (database, report) = Database::open(file(&dir), dir.path()).expect("open the database");

    assert!(report.bootstrapped, "the empty file is bootstrapped");
    assert!(report.applied.is_empty(), "bootstrap records migrations without applying them");
    assert!(report.warnings.is_empty(), "no migration warned");
    assert_eq!(MIGRATIONS.len(), 64);
    assert_eq!(count(&database, "SELECT count(*) FROM migration"), MIGRATIONS.len() as i64);
    assert!(exists(&database, "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'session_v2'"));
    assert!(!exists(&database, "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'session_pending'"));
}

#[test]
fn a_second_open_applies_nothing() {
    let dir = TempDir::new();
    let (database, report) = Database::open(file(&dir), dir.path()).expect("open the database");
    assert!(report.bootstrapped, "the empty file is bootstrapped");
    drop(database);

    let (_database, report) = Database::open(file(&dir), dir.path()).expect("reopen the database");
    assert!(!report.bootstrapped, "the second open does not bootstrap");
    assert!(report.applied.is_empty(), "the second open applies nothing");
}

#[test]
fn underscore_tables_are_ignored() {
    let dir = TempDir::new();
    seed(&dir.path().join("opencode.db"), "CREATE TABLE _embedder_state (id text PRIMARY KEY);");

    let (database, report) = Database::open(file(&dir), dir.path()).expect("open the database");
    assert!(report.bootstrapped, "the underscore table does not make the database non-empty");
    assert!(exists(&database, "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'session_v2'"));
}

#[test]
fn a_non_empty_database_without_a_session_table_is_rejected() {
    let dir = TempDir::new();
    seed(&dir.path().join("opencode.db"), "CREATE TABLE unrelated (id text PRIMARY KEY);");

    match Database::open(file(&dir), dir.path()) {
        Ok(_) => panic!("a non-empty database without a session table is rejected"),
        Err(error) => assert_eq!(error.to_string(), "Database is not empty and has no session table"),
    }
}

#[test]
fn a_failing_probe_migration_rolls_back_and_records_nothing() {
    let dir = TempDir::new();
    let (database, _) = Database::open(file(&dir), dir.path()).expect("open the database");
    let input = [Migration {
        id: "29990101000000_probe",
        foreign_keys: true,
        body: Body::Sql(&["CREATE TABLE probe (id integer)", "NOT SQL"]),
    }];

    assert!(apply_only(&database, &input, None).is_err(), "the invalid statement fails the migration");
    assert!(
        !exists(&database, "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'probe'"),
        "the created table is rolled back"
    );
    assert!(!exists(&database, "SELECT count(*) FROM migration WHERE id = '29990101000000_probe'"));
}

const PROBE_INSERT: &str = "INSERT INTO project_directory (project_id, directory, type, strategy, time_created) VALUES ('missing', '/tmp/probe', NULL, NULL, 0)";
static PROBE_INSERT_ONLY: [&str; 1] = [PROBE_INSERT];
static PROBE_INSERT_THEN_FAIL: [&str; 2] = [PROBE_INSERT, "NOT SQL"];

#[test]
fn a_foreign_key_free_migration_may_violate_foreign_keys() {
    let dir = TempDir::new();
    let (database, _) = Database::open(file(&dir), dir.path()).expect("open the database");
    let input = [Migration { id: "29990101000000_probe", foreign_keys: false, body: Body::Sql(&PROBE_INSERT_ONLY) }];

    let report = apply_only(&database, &input, None).expect("foreign keys are disabled for the migration");
    assert_eq!(report.applied, vec!["29990101000000_probe"]);
    assert_eq!(integer(&database, "PRAGMA foreign_keys"), 1, "foreign keys are restored after the migration");
    assert!(exists(&database, "SELECT count(*) FROM project_directory WHERE project_id = 'missing'"));
}

#[test]
fn a_failing_foreign_key_free_migration_restores_foreign_keys() {
    let dir = TempDir::new();
    let (database, _) = Database::open(file(&dir), dir.path()).expect("open the database");
    let input =
        [Migration { id: "29990101000000_probe", foreign_keys: false, body: Body::Sql(&PROBE_INSERT_THEN_FAIL) }];

    assert!(apply_only(&database, &input, None).is_err(), "the invalid statement fails the migration");
    assert_eq!(integer(&database, "PRAGMA foreign_keys"), 1, "foreign keys are restored after the failure");
    assert!(!exists(&database, "SELECT count(*) FROM project_directory WHERE project_id = 'missing'"));
}

#[test]
fn a_failing_pending_migration_rolls_back_and_records_nothing() {
    let dir = TempDir::new();
    let (database, _) = Database::open(file(&dir), dir.path()).expect("open the database");
    database
        .read(|connection| {
            connection.execute("DELETE FROM migration WHERE id = '20261120004954_machine-key'", [])?;
            Ok(())
        })
        .expect("delete the pending migration");
    drop(database);

    let error = match Database::open(file(&dir), dir.path()) {
        Ok(_) => panic!("the reopen applies the deleted migration and fails"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("duplicate column name"), "the alter fails: {error}");

    let connection = rusqlite::Connection::open(dir.path().join("opencode.db")).expect("open a raw connection");
    let recorded: i64 = connection
        .query_row("SELECT count(*) FROM migration WHERE id = '20261120004954_machine-key'", [], |row| row.get(0))
        .expect("count the recorded migration");
    assert_eq!(recorded, 0, "the failed migration is not recorded");
}

#[test]
fn a_named_drizzle_journal_is_imported_once() {
    let dir = TempDir::new();
    let (database, _) = Database::open(file(&dir), dir.path()).expect("open the database");
    database
        .read(|connection| {
            connection.execute_batch(
                "CREATE TABLE __drizzle_migrations (id INTEGER PRIMARY KEY, hash text NOT NULL, created_at numeric, name text, applied_at TEXT);
                 INSERT INTO __drizzle_migrations (hash, created_at, name, applied_at) VALUES ('hash', 1, 'legacy', 'now');
                 DELETE FROM migration;",
            )?;
            Ok(())
        })
        .expect("seed the legacy journal");

    apply_only(&database, &[], None).expect("import the journal");
    assert_eq!(completed_ids(&database), vec!["legacy"], "the named journal is imported");

    database
        .read(|connection| {
            connection.execute_batch(
                "INSERT INTO migration (id, time_completed) VALUES ('existing', 1);
                 UPDATE __drizzle_migrations SET name = 'ignored';",
            )?;
            Ok(())
        })
        .expect("record an existing migration");

    apply_only(&database, &[], None).expect("skip the imported journal");
    assert_eq!(completed_ids(&database), vec!["existing", "legacy"], "the journal is not re-imported");
}

#[test]
fn an_unnamed_drizzle_journal_matches_by_timestamp() {
    let dir = TempDir::new();
    let (database, _) = Database::open(file(&dir), dir.path()).expect("open the database");
    database
        .read(|connection| {
            connection.execute_batch(
                "DELETE FROM migration;
                 CREATE TABLE __drizzle_migrations (id integer PRIMARY KEY, hash text, created_at integer);
                 INSERT INTO __drizzle_migrations (created_at) VALUES (1775843113000);",
            )?;
            Ok(())
        })
        .expect("seed the unnamed journal");

    let input = [Migration { id: "20260410174513_workspace-name", foreign_keys: true, body: Body::Sql(&[]) }];
    apply_only(&database, &input, None).expect("import the timestamped journal");
    assert_eq!(completed_ids(&database), vec!["20260410174513_workspace-name"]);

    apply_only(&database, &input, None).expect("skip the imported journal");
    assert_eq!(
        completed_ids(&database),
        vec!["20260410174513_workspace-name"],
        "the timestamped journal is imported once"
    );
    assert!(
        !exists(&database, "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = '__new_workspace'"),
        "the empty body changes no schema"
    );
}

#[test]
fn an_unknown_drizzle_timestamp_is_rejected() {
    let dir = TempDir::new();
    let (database, _) = Database::open(file(&dir), dir.path()).expect("open the database");
    database
        .read(|connection| {
            connection.execute_batch(
                "DELETE FROM migration;
                 CREATE TABLE __drizzle_migrations (id integer PRIMARY KEY, hash text, created_at integer);
                 INSERT INTO __drizzle_migrations (created_at) VALUES (1234567890000);",
            )?;
            Ok(())
        })
        .expect("seed the unnamed journal");

    let input = [Migration { id: "20260410174513_workspace-name", foreign_keys: true, body: Body::Sql(&[]) }];
    let error = match apply_only(&database, &input, None) {
        Ok(_) => panic!("the unknown timestamp is rejected"),
        Err(error) => error,
    };
    assert_eq!(error.to_string(), "Legacy migration timestamp 1234567890000 does not match any known migration");
}

#[test]
fn a_rejected_drizzle_journal_imports_nothing() {
    let dir = TempDir::new();
    let (database, _) = Database::open(file(&dir), dir.path()).expect("open the database");
    database
        .read(|connection| {
            connection.execute_batch(
                "DELETE FROM migration;
                 CREATE TABLE __drizzle_migrations (id integer PRIMARY KEY, hash text, created_at integer);
                 INSERT INTO __drizzle_migrations (created_at) VALUES (1775843113000), (1234567890000);",
            )?;
            Ok(())
        })
        .expect("seed the unnamed journal");

    let input = [Migration { id: "20260410174513_workspace-name", foreign_keys: true, body: Body::Sql(&[]) }];
    assert!(apply_only(&database, &input, None).is_err(), "the unknown timestamp is rejected");
    assert!(completed_ids(&database).is_empty(), "the matched entry is rolled back with the rejected one");
}

#[test]
fn unknown_migration_ids_are_tolerated() {
    let dir = TempDir::new();
    let (database, _) = Database::open(file(&dir), dir.path()).expect("open the database");
    database
        .read(|connection| {
            connection.execute_batch(
                "DELETE FROM migration;
                 INSERT INTO migration (id, time_completed) VALUES ('99999999999999_unknown', 1);",
            )?;
            Ok(())
        })
        .expect("record an unknown migration");

    let report = apply_only(&database, &[], None).expect("tolerate the unknown migration id");
    assert!(report.applied.is_empty(), "the unknown id applies nothing");
    assert!(exists(&database, "SELECT count(*) FROM migration WHERE id = '99999999999999_unknown'"));
}

#[test]
fn foreign_keys_are_restored_after_the_rebuild_migration() {
    let dir = TempDir::new();
    let (database, _) = Database::open(file(&dir), dir.path()).expect("open the database");
    database
        .read(|connection| {
            connection.execute("DELETE FROM migration WHERE id = '20260823191254_nullable_workspace_binding'", [])?;
            Ok(())
        })
        .expect("delete the rebuild migration");
    drop(database);

    let (database, report) = Database::open(file(&dir), dir.path()).expect("reopen the database");
    assert!(!report.bootstrapped, "the database is already bootstrapped");
    assert_eq!(report.applied, vec!["20260823191254_nullable_workspace_binding"]);
    assert_eq!(integer(&database, "PRAGMA foreign_keys"), 1, "the rebuild restores foreign key enforcement");
    assert!(exists(&database, "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'workspace'"));
}

#[test]
fn hand_ported_ids_match_the_generated_registry() {
    let registered: Vec<&str> = MIGRATIONS
        .iter()
        .filter(|migration| matches!(&migration.body, Body::HandPorted))
        .map(|migration| migration.id)
        .collect();
    assert_eq!(registered, HAND_PORTED.to_vec());
    assert_eq!(HAND_PORTED.len(), 4);
}

#[test]
fn legacy_credentials_are_imported_on_file_bootstrap() {
    let _guard = crate::lock_identifiers();
    let dir = TempDir::new();
    std::fs::write(dir.path().join("auth.json"), LEGACY_CREDENTIALS).expect("write the legacy credential file");
    let (database, _) = Database::open(file(&dir), dir.path()).expect("open the database");

    let rows = credentials(&database);
    assert_eq!(
        credential_rows(&rows),
        vec![
            ("anthropic", "API key", r#"{"type":"key","key":"legacy-key","metadata":{"region":"us"}}"#),
            ("custom-provider", "API key", r#"{"type":"key","key":"custom-key"}"#),
            (
                "github-copilot",
                "OAuth",
                r#"{"type":"oauth","methodID":"device","refresh":"refresh","access":"access","expires":123}"#,
            ),
            ("google", "API key", r#"{"type":"key","key":"google-key","metadata":{"region":"us"}}"#),
            ("https://example.com", "API key", r#"{"type":"key","key":"wellknown-key"}"#),
            (
                "openai",
                "OAuth",
                r#"{"type":"oauth","methodID":"chatgpt-browser","refresh":"refresh","access":"access","expires":123,"metadata":{"accountID":"account"}}"#,
            ),
        ]
    );
    for (id, _, _, _) in &rows {
        assert_credential_id(id);
    }
    assert_eq!(value(&database, "SELECT value FROM kv WHERE key = 'wellknown:sources'"), r#"["https://example.com"]"#);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("auth.json")).expect("read the legacy credential file"),
        LEGACY_CREDENTIALS,
        "the source file is unchanged"
    );

    database
        .read(|connection| {
            connection.execute_batch(
                "DELETE FROM migration WHERE id = '20260805200742_import_legacy_credentials';
                 DELETE FROM credential;
                 INSERT INTO credential (id, integration_id, label, value, time_created, time_updated)
                 VALUES ('existing', 'anthropic', 'Existing', '{\"type\":\"key\",\"key\":\"current-key\"}', 1, 1);
                 INSERT INTO kv (key, value, time_created, time_updated)
                 VALUES ('wellknown:sources', '[\"https://old\"]', 1, 1)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value;",
            )?;
            Ok(())
        })
        .expect("replace the imported credentials");
    drop(database);

    let (database, report) = Database::open(file(&dir), dir.path()).expect("reopen the database");
    assert_eq!(report.applied, vec!["20260805200742_import_legacy_credentials"]);
    assert_eq!(
        credential_rows(&credentials(&database)),
        vec![
            ("anthropic", "Existing", r#"{"type":"key","key":"current-key"}"#),
            ("custom-provider", "API key", r#"{"type":"key","key":"custom-key"}"#),
            (
                "github-copilot",
                "OAuth",
                r#"{"type":"oauth","methodID":"device","refresh":"refresh","access":"access","expires":123}"#,
            ),
            ("google", "API key", r#"{"type":"key","key":"google-key","metadata":{"region":"us"}}"#),
            ("https://example.com", "API key", r#"{"type":"key","key":"wellknown-key"}"#),
            (
                "openai",
                "OAuth",
                r#"{"type":"oauth","methodID":"chatgpt-browser","refresh":"refresh","access":"access","expires":123,"metadata":{"accountID":"account"}}"#,
            ),
        ],
        "the existing credential is kept and the other five are imported"
    );
    assert_eq!(
        value(&database, "SELECT value FROM kv WHERE key = 'wellknown:sources'"),
        r#"["https://old","https://example.com"]"#,
        "the well-known origins are merged"
    );
}

#[test]
fn legacy_credentials_are_skipped_for_memory_databases() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("auth.json"), LEGACY_CREDENTIALS).expect("write the legacy credential file");

    let (database, report) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    assert!(report.bootstrapped, "the memory database is bootstrapped");
    assert_eq!(count(&database, "SELECT count(*) FROM credential"), 0, "an in-memory database keeps no credentials");
}

#[test]
fn a_malformed_legacy_credential_file_bootstraps_with_a_warning() {
    let _guard = crate::lock_identifiers();
    let dir = TempDir::new();
    std::fs::write(dir.path().join("auth.json"), "not json").expect("write the malformed credential file");

    let (database, report) = Database::open(file(&dir), dir.path()).expect("open the database");
    assert!(report.bootstrapped, "the malformed file does not stop the bootstrap");
    assert_eq!(
        report.warnings,
        vec![format!(
            "Skipped legacy credential file that is not a JSON object: {}",
            dir.path().join("auth.json").display()
        )]
    );
    assert_eq!(count(&database, "SELECT count(*) FROM credential"), 0);
}

#[test]
fn an_absent_legacy_credential_file_imports_nothing() {
    let dir = TempDir::new();

    let (database, report) = Database::open(file(&dir), dir.path()).expect("open the database");
    assert!(report.warnings.is_empty(), "an absent file warns about nothing");
    assert_eq!(count(&database, "SELECT count(*) FROM credential"), 0);
}
