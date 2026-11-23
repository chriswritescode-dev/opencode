import { Database as BunDatabase } from "bun:sqlite"
import { pathToFileURL } from "node:url"
import { upstreamCoreTree } from "../upstream"

export type Transaction = {
  run: (statement: unknown) => unknown
  get: (statement: unknown) => unknown
  all: (statement: unknown) => unknown
}

export type Migration = {
  id: string
  foreignKeys?: boolean
  up: (tx: Transaction) => unknown
}

const resolve = (tree: string, name: string) =>
  import(pathToFileURL(Bun.resolveSync(name, `${tree}/packages/core/src`)).href)

const load = (tree: string, path: string) => import(pathToFileURL(`${tree}/packages/${path}`).href)

export async function loadMigrations() {
  const tree = await upstreamCoreTree()
  const effect = await resolve(tree, "effect")
  const global = await load(tree, "util/src/global.ts")
  const migration = await load(tree, "core/src/database/migration.gen.ts")
  const schema = await load(tree, "core/src/database/schema.gen.ts")
  const sqlite = await resolve(tree, "@effect/sql-sqlite-bun")
  const drizzle = await resolve(tree, "drizzle-orm")
  const database = await load(tree, "core/src/database/database.ts")
  const databaseMigration = await load(tree, "core/src/database/migration.ts")
  const drizzleSqlite = await load(tree, "core/src/database/drizzle/index.ts")
  return {
    Effect: effect.Effect,
    Layer: effect.Layer,
    References: effect.References,
    Global: global.Global,
    SqliteClient: sqlite.SqliteClient,
    sql: drizzle.sql,
    Database: database.Database,
    DatabaseMigration: databaseMigration.DatabaseMigration,
    EffectDrizzleSqlite: drizzleSqlite.EffectDrizzleSqlite,
    migrations: migration.migrations as ReadonlyArray<Migration>,
    schema: schema.default as Migration,
  }
}

export async function openTypescript(file: string, data: string): Promise<void> {
  const { Effect, Layer, References, Global, Database } = await loadMigrations()
  await Effect.runPromise(
    Effect.gen(function* () {
      yield* Layer.build(Database.layer({ path: file }))
    }).pipe(
      Effect.scoped,
      Effect.provideService(Global.Service, Global.make({ data })),
      Effect.provideService(References.MinimumLogLevel, "None"),
    ),
  )
}

export type StartStep = { apply: number } | { sql: string }

const quoteIdentifier = (name: string) => `"${name.replaceAll('"', '""')}"`

export async function buildStart(steps: StartStep[]): Promise<string[]> {
  const { Effect, References, Global, SqliteClient, sql, DatabaseMigration, EffectDrizzleSqlite, migrations } =
    await loadMigrations()
  const makeDb = EffectDrizzleSqlite.makeWithDefaults()
  return Effect.runPromise(
    Effect.gen(function* () {
      const db = yield* makeDb
      for (const step of steps) {
        if ("sql" in step) yield* db.run(sql.raw(step.sql))
        else yield* DatabaseMigration.applyOnly(db, migrations.slice(0, step.apply))
      }
      const client = db.$client
      const journal = yield* client.unsafe("SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'migration'")
        .values
      if (journal.length > 0) yield* client.unsafe('UPDATE "migration" SET time_completed = 0').values

      const tables = (yield* client.unsafe(
        "SELECT name, sql FROM sqlite_master WHERE type = 'table' AND substr(name, 1, 7) <> 'sqlite_' ORDER BY rowid",
      ).values) as ReadonlyArray<ReadonlyArray<string>>
      const statements = tables.map((row) => row[1])
      for (const table of tables) {
        const columns = (
          (yield* client.unsafe(`PRAGMA table_info(${quoteIdentifier(table[0])})`).values) as ReadonlyArray<
            ReadonlyArray<string>
          >
        ).map((row) => row[1])
        if (columns.length === 0) continue
        const cells = columns.map((column) => `quote(${quoteIdentifier(column)})`).join(" || ',' || ")
        const rows = (yield* client.unsafe(`SELECT ${cells} FROM ${quoteIdentifier(table[0])}`)
          .values) as ReadonlyArray<ReadonlyArray<string>>
        for (const row of rows) statements.push(`INSERT INTO ${quoteIdentifier(table[0])} VALUES (${row[0]})`)
      }
      const others = (yield* client.unsafe(
        "SELECT sql FROM sqlite_master WHERE type IN ('index','trigger','view') AND sql IS NOT NULL ORDER BY rowid",
      ).values) as ReadonlyArray<ReadonlyArray<string>>
      for (const row of others) statements.push(row[0])
      return statements
    }).pipe(
      Effect.provideService(Global.Service, Global.make({ data: "/nonexistent-opencode-storage-corpus" })),
      Effect.provideService(References.MinimumLogLevel, "None"),
      Effect.provide(SqliteClient.layer({ filename: ":memory:", disableWAL: true })),
      Effect.scoped,
    ),
  )
}

type Cell = null | string | [string, string]

export function dump(db: BunDatabase, masks: Record<string, string[]>): unknown {
  const schema = db.query("SELECT type, name, tbl_name, sql FROM sqlite_master ORDER BY type, name").values() as Array<
    [string, string, string, string | null]
  >

  const names = (
    db
      .query("SELECT name FROM sqlite_master WHERE type = 'table' AND substr(name, 1, 7) <> 'sqlite_' ORDER BY name")
      .values() as Array<[string]>
  ).map((row) => row[0])

  const tables: Record<string, unknown> = {}
  for (const table of names) {
    const columns = (db.query(`PRAGMA table_info(${quoteIdentifier(table)})`).values() as Array<unknown[]>).map(
      (row) => row[1] as string,
    )
    const masked = new Set(masks[table] ?? [])
    const select = columns
      .map(
        (column) =>
          `typeof(${quoteIdentifier(column)}), CASE typeof(${quoteIdentifier(column)}) WHEN 'blob' THEN hex(${quoteIdentifier(column)}) ELSE ${quoteIdentifier(column)} END`,
      )
      .join(", ")
    const raw =
      columns.length === 0
        ? []
        : (db.query(`SELECT ${select} FROM ${quoteIdentifier(table)}`).values() as Array<unknown[]>)
    const rows = raw.map((row) => {
      const cells: Cell[] = []
      for (const [index, column] of columns.entries()) {
        const type = row[index * 2]
        const value = row[index * 2 + 1]
        if (value === null || value === undefined) cells.push(null)
        else if (masked.has(column)) cells.push("<masked>")
        else if (type === "integer" || type === "real") cells.push([type, String(value)])
        else cells.push([type as string, value as string])
      }
      return cells
    })
    rows.sort((left, right) => {
      const a = JSON.stringify(left)
      const b = JSON.stringify(right)
      return a < b ? -1 : a > b ? 1 : 0
    })
    tables[table] = { columns, rows }
  }

  const foreignKeyViolations = db.query("PRAGMA foreign_key_check").values().length

  return { schema, tables, foreignKeyViolations }
}
