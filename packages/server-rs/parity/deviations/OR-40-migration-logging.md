# OR-40: migrations log nothing and return a report instead

TypeScript: `packages/core/src/database/migration.ts` logs through `Effect.logInfo` and `Effect.logError`:
"database schema bootstrap started" with the migration count at `:35`, "database schema bootstrap completed" with
the count and `durationMs` at `:50-53`, "database migration started" with the migration id at `:113`, "database
migration completed" with the id and `durationMs` at `:149-152`, and "database migration failed" with the id,
`durationMs` and the error from `Effect.tapError` at `:140-148`.
`packages/core/src/database/migration/20260805200742_import_legacy_credentials.ts:50-53` logs the warning "Skipped
legacy credential file that is not a JSON object" with `{ path: filepath }` when the decoded file is not an object.
A failure aborts the effect rather than returning an error: the non-empty database without a session table and the
unmatched Drizzle timestamp `Effect.die` (`:33`, `:94-96`), and the foreign-key restore runs under `Effect.orDie`
(`:138`).

Rust: `crates/storage/src/migration/mod.rs` and `crates/storage/src/migration/hand.rs` write no log lines and take
no logger. `Database::open` (`crates/storage/src/database.rs:20`) returns `(Database, MigrationReport)`, whose
`bootstrapped`, `applied` and `warnings` fields (`crates/storage/src/migration/mod.rs:17-21`) carry what the
TypeScript logs described: whether the schema was bootstrapped, the ids of the migrations applied by this call, and
the skipped-file warning as one message with the path appended (`crates/storage/src/migration/hand.rs:105`), since
a Rust string has no structured fields for the TypeScript `{ path }` yet. No durations are reported. A failure
returns `Err(StorageError)`: `StorageError::Migration("Database is not empty and has no session table")`
(`crates/storage/src/migration/mod.rs:34`) and the unmatched legacy timestamp
(`crates/storage/src/migration/mod.rs:129-132`), which `Database::open` propagates.

Reason: server-rs has no structured logger yet, which OR-39-logging records; the task that wires `Database::open`
into the server owns turning the report into those log lines and the error into the startup failure, so storage does
not take a logging dependency now. `crates/storage/tests/all/migration.rs` is the check: `bootstraps_an_empty_file`
and `a_second_open_applies_nothing` assert `bootstrapped` with an empty `applied` and `warnings`, the upgrade tests
assert the applied ids, `a_malformed_legacy_credential_file_bootstraps_with_a_warning` asserts the exact warning text
with the `auth.json` path, and `an_absent_legacy_credential_file_imports_nothing` asserts no warning.

Accepted: pending owner
