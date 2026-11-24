pub mod hand;

use std::collections::HashSet;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};

use crate::StorageError;
use crate::database::Database;
use crate::generated::migrations::{Body, MIGRATIONS, Migration};
use crate::generated::schema::SCHEMA;
use crate::id;

pub const MIGRATION_TABLE: &str = r#"CREATE TABLE "migration" (id TEXT PRIMARY KEY, time_completed INTEGER NOT NULL)"#;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    pub bootstrapped: bool,
    pub applied: Vec<&'static str>,
    pub warnings: Vec<String>,
}

pub(crate) fn apply(db: &Database, legacy_credentials: Option<&Path>) -> Result<MigrationReport, StorageError> {
    let report = db.transaction(TransactionBehavior::Immediate, |tx| {
        let mut statement = tx.prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND substr(name, 1, 1) <> '_'",
        )?;
        let tables = statement.query_map([], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        if tables.iter().any(|name| name == "session" || name == "session_v2") {
            return Ok(None);
        }
        if !tables.is_empty() {
            return Err(StorageError::Migration("Database is not empty and has no session table".into()));
        }
        let mut report = MigrationReport::default();
        for statement in SCHEMA {
            tx.execute(statement, [])?;
        }
        if let Some(auth_json) = legacy_credentials {
            hand::import_legacy_credentials(tx, auth_json, &mut report)?;
        }
        tx.execute(MIGRATION_TABLE, [])?;
        for migration in MIGRATIONS {
            record(tx, migration.id, id::now_ms())?;
        }
        Ok(Some(report))
    })?;
    match report {
        Some(report) => Ok(MigrationReport { bootstrapped: true, ..report }),
        None => apply_only(db, MIGRATIONS, legacy_credentials),
    }
}

pub fn apply_only(
    db: &Database,
    migrations: &[Migration],
    legacy_credentials: Option<&Path>,
) -> Result<MigrationReport, StorageError> {
    let known: HashSet<String> = db.transaction(TransactionBehavior::Immediate, |tx| {
        tx.execute(
            r#"CREATE TABLE IF NOT EXISTS "migration" (id TEXT PRIMARY KEY, time_completed INTEGER NOT NULL)"#,
            [],
        )?;
        let known = completed(tx)?;
        if !known.is_empty() {
            return Ok::<_, StorageError>(known.into_iter().collect());
        }
        let drizzle = tx
            .query_row(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?",
                ["__drizzle_migrations"],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if drizzle.is_none() {
            return Ok(HashSet::new());
        }
        import_drizzle_journal(tx, migrations)?;
        Ok(completed(tx)?.into_iter().collect())
    })?;

    let mut report = MigrationReport::default();
    for migration in migrations {
        if known.contains(migration.id) {
            continue;
        }
        let result = if migration.foreign_keys {
            run(db, migration, legacy_credentials, &mut report)
        } else {
            db.read(|connection| {
                connection.execute_batch("PRAGMA foreign_keys = OFF")?;
                Ok(())
            })?;
            let result = run(db, migration, legacy_credentials, &mut report);
            db.read(|connection| {
                connection.execute_batch("PRAGMA foreign_keys = ON")?;
                Ok(())
            })?;
            result
        };
        if result? {
            report.applied.push(migration.id);
        }
    }
    Ok(report)
}

fn import_drizzle_journal(tx: &Transaction<'_>, migrations: &[Migration]) -> Result<(), StorageError> {
    let mut statement = tx.prepare("SELECT name FROM pragma_table_info('__drizzle_migrations')")?;
    let columns = statement.query_map([], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
    if columns.iter().any(|name| name == "name") {
        tx.execute(
            r#"INSERT OR IGNORE INTO "migration" (id, time_completed) SELECT name, ? FROM "__drizzle_migrations" WHERE name IS NOT NULL"#,
            [id::now_ms()],
        )?;
        return Ok(());
    }
    let mut statement = tx.prepare(
        r#"SELECT created_at, strftime('%Y%m%d%H%M%S', created_at / 1000, 'unixepoch') AS prefix FROM "__drizzle_migrations" WHERE created_at IS NOT NULL"#,
    )?;
    let entries = statement
        .query_map([], |row| Ok((row.get::<_, f64>(0)?, row.get::<_, Option<String>>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    for (created_at, prefix) in entries {
        let prefix = prefix.unwrap_or_else(|| "null".to_owned());
        let Some(migration) = migrations.iter().find(|migration| migration.id.starts_with(&format!("{prefix}_")))
        else {
            return Err(StorageError::Migration(format!(
                "Legacy migration timestamp {} does not match any known migration",
                opencode_api_types::js::number_to_string(created_at)
            )));
        };
        tx.execute(
            r#"INSERT OR IGNORE INTO "migration" (id, time_completed) VALUES (?, ?)"#,
            rusqlite::params![migration.id, id::now_ms()],
        )?;
    }
    Ok(())
}

fn run(
    db: &Database,
    migration: &Migration,
    legacy_credentials: Option<&Path>,
    report: &mut MigrationReport,
) -> Result<bool, StorageError> {
    db.transaction(TransactionBehavior::Immediate, |tx| {
        let recorded = tx
            .query_row(r#"SELECT 1 FROM "migration" WHERE id = ?"#, [migration.id], |row| row.get::<_, i64>(0))
            .optional()?;
        if recorded.is_some() {
            return Ok(false);
        }
        match &migration.body {
            Body::Sql(statements) => {
                for statement in *statements {
                    tx.execute_batch(statement)?;
                }
            }
            Body::HandPorted => hand::apply(migration.id, tx, legacy_credentials, report)?,
        }
        record(tx, migration.id, id::now_ms())?;
        Ok(true)
    })
}

pub fn completed(conn: &Connection) -> Result<Vec<String>, StorageError> {
    let mut statement = conn.prepare(r#"SELECT id FROM "migration""#)?;
    let ids = statement.query_map([], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
    Ok(ids)
}

fn record(tx: &Transaction<'_>, id: &str, time: i64) -> Result<(), StorageError> {
    tx.execute(r#"INSERT INTO "migration" (id, time_completed) VALUES (?, ?)"#, rusqlite::params![id, time])?;
    Ok(())
}
