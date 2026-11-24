# OR-40: migrations take the write lock up front and import the Drizzle journal atomically

TypeScript: `packages/core/src/database/migration.ts` reads the table list outside any transaction at `:28-30`
and bootstraps in a deferred `db.transaction` at `:36-49`, relying on the in-process lock the Database layer holds
(`:22-23`). `applyOnly` imports an unnamed Drizzle journal with one autocommit `INSERT OR IGNORE` per entry at
`:91-102`, so an unmatched timestamp at `:93-97` leaves the earlier entries recorded, and the next start skips the
import because the `migration` table is no longer empty (`:65`). Each pending migration runs in a deferred
transaction at `:114-121` against the completed set read before the loop. `packages/core/src/database/database.ts:44-46`
sets `journal_mode = WAL` before `busy_timeout = 5000`.

Rust: `crates/storage/src/migration/mod.rs` reads the table list and bootstraps inside one
`TransactionBehavior::Immediate` transaction in `apply`, and falls through to `apply_only` when a session table is
already present. `apply_only` creates the `migration` table, reads the completed ids and imports the Drizzle
journal (`import_drizzle_journal`) inside one immediate transaction, so an unmatched timestamp rolls back every
entry and the next start retries the import. `run` applies each pending migration in an immediate transaction that
first checks the `migration` table again and skips an id that another connection recorded in the meantime, which is
then not listed in `MigrationReport::applied`. `Database::open` (`crates/storage/src/database.rs`) sets
`busy_timeout` before `journal_mode = WAL`, so the journal-mode switch also waits for a lock held by another
connection.

Reason: the TypeScript and Rust servers can open one database file from separate processes, which the in-process
lock does not serialize. A deferred transaction that reads before writing can fail with `SQLITE_BUSY` without
honouring `busy_timeout` when it upgrades to a write lock, and a second process that saw an empty database would
fail its `CREATE TABLE` instead of upgrading the bootstrapped one. A partial journal import would make the next
start re-run migrations the legacy journal had already applied. Successful opens produce the same schema and
`migration` rows as TypeScript. `crates/storage/tests/all/migration.rs` is the check:
`a_rejected_drizzle_journal_imports_nothing` asserts that a rejected journal records no entry, and the existing
bootstrap, upgrade and journal tests assert the unchanged results.

Accepted: pending owner
