import { Database as BunDatabase } from "bun:sqlite"
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import {
  buildStart,
  dump,
  loadMigrations,
  openTypescript,
  type StartStep,
  type Transaction,
} from "../storage/typescript"

const root = new URL("../..", import.meta.url).pathname
const { Effect, Global, migrations, schema } = await loadMigrations()

const HAND_PORTED = new Set([
  "20260410174513_workspace-name",
  "20260511173437_session-metadata",
  "20260804233008_loose_psylocke",
  "20260805200742_import_legacy_credentials",
])

type Answers = {
  get?: unknown[]
  all?: unknown[]
}

const registry = new Map(migrations.map((migration) => [migration.id, migration]))
for (const id of HAND_PORTED) if (!registry.has(id)) fail(`storage: ${id} is missing from the migration registry`)

const bodies = new Map<string, string[]>()
for (const migration of migrations) {
  if (HAND_PORTED.has(migration.id)) continue
  const statements = await record(migration)
  if (statements.length === 0) fail(`storage: ${migration.id} recorded no statements`)
  bodies.set(migration.id, statements)
}

const workspaceName = handPorted("20260410174513_workspace-name")
const sessionMetadata = handPorted("20260511173437_session-metadata")
const loosePsylocke = handPorted("20260804233008_loose_psylocke")
const branches = [
  { name: "WORKSPACE_NAME_WITH_NAME", statements: await record(workspaceName, { all: [[{ name: "name" }]] }) },
  { name: "WORKSPACE_NAME_WITHOUT_NAME", statements: await record(workspaceName, { all: [[]] }) },
  { name: "SESSION_METADATA_ADD", statements: await record(sessionMetadata, { all: [[]] }) },
  {
    name: "LOOSE_PSYLOCKE_PREVIOUS_V2",
    statements: await record(loosePsylocke, {
      get: [{ id: "20260730195856_optional_session_title" }, undefined],
    }),
  },
  { name: "LOOSE_PSYLOCKE_FRESH", statements: await record(loosePsylocke, { get: [undefined] }) },
]

const schemaStatements = await record({ id: "schema", up: schema.up })

const directory = `${root}crates/storage/src/generated`
await mkdir(directory, { recursive: true })
const migrationsFile = `${directory}/migrations.rs`
const schemaFile = `${directory}/schema.rs`
const modFile = `${directory}/mod.rs`

await Bun.write(
  migrationsFile,
  `${header()}
pub struct Migration {
    pub id: &'static str,
    pub foreign_keys: bool,
    pub body: Body,
}

pub enum Body {
    Sql(&'static [&'static str]),
    HandPorted,
}

${branches.map((branch) => constant(branch.name, branch.statements)).join("\n")}
pub const MIGRATIONS: &[Migration] = &[
${migrations
  .map((migration) => {
    const statements = bodies.get(migration.id)
    const body = statements === undefined ? "Body::HandPorted" : `Body::Sql(${array(statements)})`
    return `    Migration {
        id: ${JSON.stringify(migration.id)},
        foreign_keys: ${migration.foreignKeys !== false},
        body: ${body},
    },`
  })
  .join("\n")}
];
`,
)

await Bun.write(
  schemaFile,
  `${header()}
pub const SCHEMA: &[&str] = ${array(schemaStatements)};
`,
)

await Bun.write(
  modFile,
  `${header()}

pub mod migrations;
pub mod schema;
`,
)

const format = Bun.spawnSync(["rustfmt", "--edition", "2024", migrationsFile, schemaFile, modFile], {
  stdout: "inherit",
  stderr: "inherit",
})
if (format.exitCode !== 0) process.exit(format.exitCode ?? 1)

const masks = {
  migration: ["time_completed"],
  credential: ["id", "time_created", "time_updated"],
  kv: ["time_created", "time_updated"],
}

const ids = (count: number) => migrations.slice(0, count).map((migration) => migration.id)
const timestamp = (id: string) => {
  const date = id.slice(0, 8)
  const time = id.slice(8, 14)
  return Date.UTC(
    Number(date.slice(0, 4)),
    Number(date.slice(4, 6)) - 1,
    Number(date.slice(6, 8)),
    Number(time.slice(0, 2)),
    Number(time.slice(2, 4)),
    Number(time.slice(4, 6)),
  )
}

const fixtures: Array<{ name: string; authJson: string | null; steps: StartStep[] }> = [
  { name: "empty", authJson: null, steps: [] },
  {
    name: "empty-legacy-credentials",
    authJson: JSON.stringify({
      anthropic: { type: "api", key: "legacy-key", metadata: { region: "us" } },
      openai: { type: "oauth", refresh: "refresh", access: "access", expires: 123, accountId: "account" },
      "github-copilot/": { type: "oauth", refresh: "refresh", access: "access", expires: 123 },
      "https://example.com": { type: "wellknown", key: "TOKEN", token: "wellknown-key" },
      invalid: { type: "api" },
      "/": { type: "api", key: "ignored" },
    }),
    steps: [],
  },
  { name: "empty-malformed-credentials", authJson: "[1]", steps: [] },
  {
    name: "drizzle-named-journal",
    authJson: null,
    steps: [
      { apply: 21 },
      {
        sql: `INSERT INTO project (id, worktree, sandboxes, time_created, time_updated) VALUES ('prj_legacy', '/repo', '[]', 1, 2)`,
      },
      {
        sql: `INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated) VALUES ('ses_legacy', 'prj_legacy', 'slug', '/repo', 'Legacy', '1', 1, 2)`,
      },
      {
        sql: `INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES ('msg_legacy', 'ses_legacy', 1, 2, '{}')`,
      },
      {
        sql: `INSERT INTO workspace (id, type, name, branch, directory, project_id, time_used) VALUES ('wrk_legacy', 'local', 'main', 'main', '/repo', 'prj_legacy', 1)`,
      },
      { sql: `DROP TABLE migration` },
      {
        sql: `CREATE TABLE __drizzle_migrations (id integer PRIMARY KEY, hash text NOT NULL, created_at numeric, name text)`,
      },
      {
        sql: `INSERT INTO __drizzle_migrations (hash, created_at, name) VALUES ${ids(21)
          .map((id) => `('hash', 1, '${id}')`)
          .join(", ")}`,
      },
    ],
  },
  {
    name: "drizzle-unnamed-journal",
    authJson: null,
    steps: [
      { apply: 10 },
      {
        sql: `INSERT INTO project (id, worktree, sandboxes, time_created, time_updated) VALUES ('prj_legacy', '/repo', '[]', 1, 2)`,
      },
      {
        sql: `INSERT INTO workspace (id, type, name, branch, directory, project_id) VALUES ('wrk_legacy', 'local', 'main', 'main', '/repo', 'prj_legacy')`,
      },
      { sql: `DROP TABLE migration` },
      { sql: `CREATE TABLE __drizzle_migrations (id integer PRIMARY KEY, hash text, created_at integer)` },
      {
        sql: `INSERT INTO __drizzle_migrations (created_at) VALUES ${ids(10)
          .map((id) => `(${timestamp(id)})`)
          .join(", ")}`,
      },
    ],
  },
  {
    name: "unknown-legacy-timestamp",
    authJson: null,
    steps: [
      { sql: `CREATE TABLE session (id text PRIMARY KEY)` },
      { sql: `CREATE TABLE __drizzle_migrations (id integer PRIMARY KEY, hash text, created_at integer)` },
      { sql: `INSERT INTO __drizzle_migrations (created_at) VALUES (1234567890000)` },
    ],
  },
  {
    name: "session-metadata-present",
    authJson: null,
    steps: [{ apply: 20 }, { sql: `ALTER TABLE session ADD metadata text` }],
  },
  {
    name: "before-v2-split",
    authJson: JSON.stringify({ "custom-provider": { type: "api", key: "custom-key" } }),
    steps: [
      { apply: 38 },
      {
        sql: `INSERT INTO project (id, worktree, sandboxes, time_created, time_updated) VALUES ('prj_1', '/repo', '[]', 1, 2)`,
      },
      {
        sql: `INSERT INTO project_directory (project_id, directory, type, strategy, time_created) VALUES ('prj_1', '/repo', 'main', null, 1)`,
      },
      {
        sql: `INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated) VALUES ('ses_1', 'prj_1', 'slug', '/repo', 'Title', '1', 1, 2)`,
      },
      {
        sql: `INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES ('msg_1', 'ses_1', 1, 2, '{}')`,
      },
      {
        sql: `INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data) VALUES ('smsg_1', 'ses_1', 'text', 1, 1, 2, '{}')`,
      },
      {
        sql: `INSERT INTO workspace (id, type, name, branch, directory, project_id, time_used) VALUES ('wrk_1', 'local', 'main', 'main', '/repo', 'prj_1', 1)`,
      },
      {
        sql: `INSERT INTO credential (id, integration_id, label, value, time_created, time_updated) VALUES ('cli', 'azure', 'Azure CLI', '{"type":"oauth","methodID":"azure-cli","access":"token","refresh":"azure-cli","expires":123,"metadata":{"resourceName":"my-models"}}', 1, 2)`,
      },
    ],
  },
  {
    name: "previous-v2",
    authJson: null,
    steps: [
      { apply: 38 },
      { sql: `ALTER TABLE session ADD COLUMN time_suspended integer` },
      { sql: `ALTER TABLE event ADD COLUMN created integer DEFAULT 0 NOT NULL` },
      {
        sql: `CREATE TABLE IF NOT EXISTS kv (key text PRIMARY KEY, value text NOT NULL, time_created integer NOT NULL, time_updated integer NOT NULL)`,
      },
      {
        sql: `INSERT INTO project (id, worktree, sandboxes, time_created, time_updated) VALUES ('project', '/repo', '[]', 1, 2)`,
      },
      {
        sql: `INSERT INTO project_directory (project_id, directory, type, strategy, time_created) VALUES ('project', '/repo', 'main', null, 1)`,
      },
      {
        sql: `INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated) VALUES ('session', 'project', 'slug', '/repo', 'Title', '1', 1, 2)`,
      },
      {
        sql: `INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES ('message', 'session', 1, 2, '{"text":"preserved"}')`,
      },
      {
        sql: `INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data) VALUES ('message', 'session', 'text', 1, 1, 2, '{"text":"preserved"}')`,
      },
      { sql: `INSERT INTO event_sequence (aggregate_id, seq) VALUES ('session', 41)` },
      {
        sql: `INSERT INTO event (id, aggregate_id, seq, type, data, created) VALUES ('event', 'session', 41, 'session.text.ended.1', '{}', 1)`,
      },
      { sql: `INSERT INTO migration (id, time_completed) VALUES ('20260730195856_optional_session_title', 1)` },
    ],
  },
  {
    name: "previous-v2-with-v1-history",
    authJson: null,
    steps: [
      { apply: 38 },
      { sql: `ALTER TABLE session ADD COLUMN time_suspended integer` },
      { sql: `ALTER TABLE event ADD COLUMN created integer DEFAULT 0 NOT NULL` },
      {
        sql: `INSERT INTO project (id, worktree, sandboxes, time_created, time_updated) VALUES ('project', '/repo', '[]', 1, 2)`,
      },
      {
        sql: `INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated) VALUES ('session', 'project', 'slug', '/repo', 'Title', '1', 1, 2)`,
      },
      {
        sql: `INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES ('message', 'session', 1, 2, '{}')`,
      },
      { sql: `INSERT INTO migration (id, time_completed) VALUES ('20260730195856_optional_session_title', 1)` },
    ],
  },
  {
    name: "pre-automation",
    authJson: null,
    steps: [
      { apply: 59 },
      {
        sql: `INSERT INTO project (id, worktree, sandboxes, time_created, time_updated) VALUES ('prj_1', '/repo', '[]', 1, 2)`,
      },
      {
        sql: `INSERT INTO session_v2 (id, project_id, slug, directory, version, time_created, time_updated) VALUES ('ses_1', 'prj_1', 'slug', '/repo', '1', 1, 2)`,
      },
      {
        sql: `INSERT INTO schedule (id, session_id, name, action, "when", tz, status, next_due_at, remaining_runs, "check", run_count, last_message_id, time_created, time_updated) VALUES ('sch_cron', 'ses_1', 'morning', '{"type":"prompt","prompt":"check the build"}', '{"type":"cron","cron":"0 9 * * *"}', 'UTC', 'active', 60000, null, null, 2, 'msg_sch_cron_2', 1, 2)`,
      },
    ],
  },
  {
    name: "newer-build",
    authJson: null,
    steps: [{ apply: 64 }, { sql: `INSERT INTO migration (id, time_completed) VALUES ('29991231235959_future', 1)` }],
  },
  {
    name: "not-empty",
    authJson: null,
    steps: [{ sql: `CREATE TABLE other (id text)` }],
  },
  {
    name: "embedder-tables",
    authJson: null,
    steps: [{ sql: `CREATE TABLE _embedder (id text)` }],
  },
]

const corpusDirectory = `${root}crates/storage/tests/corpus`
await mkdir(corpusDirectory, { recursive: true })
const fixturesFile = `${corpusDirectory}/migrations.json`

const records = []
for (const fixture of fixtures) {
  const start = await buildStart(fixture.steps)
  const directory = await mkdtemp(join(tmpdir(), "opencode-storage-corpus-"))
  try {
    if (fixture.authJson !== null) await writeFile(join(directory, "auth.json"), fixture.authJson)
    const path = join(directory, "opencode.db")
    const created = new BunDatabase(path, { create: true })
    for (const statement of start) created.run(statement)
    created.close()
    try {
      await openTypescript(path, directory)
    } catch (caught) {
      records.push({
        name: fixture.name,
        authJson: fixture.authJson,
        start,
        expected: null,
        error: caught instanceof Error ? caught.message : String(caught),
      })
      continue
    }
    const opened = new BunDatabase(path)
    try {
      records.push({
        name: fixture.name,
        authJson: fixture.authJson,
        start,
        expected: dump(opened, masks),
        error: null,
      })
    } finally {
      opened.close()
    }
  } finally {
    await rm(directory, { recursive: true, force: true })
  }
}

await writeFile(fixturesFile, JSON.stringify({ masks, fixtures: records }, null, 2) + "\n")

console.log(
  `storage: ${migrations.length} migrations, ${HAND_PORTED.size} hand-ported, ${schemaStatements.length} schema statements, ${records.length} corpus fixtures`,
)

function handPorted(id: string) {
  const migration = registry.get(id)
  if (migration === undefined) fail(`storage: ${id} is missing from the migration registry`)
  return migration
}

async function record(
  migration: { readonly id: string; readonly up: (tx: Transaction) => unknown },
  answers: Answers = {},
) {
  const statements: string[] = []
  const gets = [...(answers.get ?? [])]
  const alls = [...(answers.all ?? [])]
  const tx: Transaction = {
    run(statement) {
      if (typeof statement !== "string")
        throw new Error(`storage: ${migration.id} ran a statement that is not a string`)
      statements.push(statement)
      return Effect.void
    },
    get() {
      if (gets.length === 0) throw new Error(`storage: ${migration.id} had no recorded get answer`)
      return Effect.succeed(gets.shift())
    },
    all() {
      if (alls.length === 0) throw new Error(`storage: ${migration.id} had no recorded all answer`)
      return Effect.succeed(alls.shift())
    },
  }
  try {
    const program = migration.up(tx) as { pipe: (apply: unknown) => unknown }
    await Effect.runPromise(program.pipe(Effect.provideService(Global.Service, Global.make())))
  } catch (error) {
    fail(`storage: ${migration.id} threw: ${error instanceof Error ? error.message : String(error)}`)
  }
  return statements
}

function array(statements: string[]) {
  return `&[
${statements.map((statement) => `    ${raw(statement)},`).join("\n")}
]`
}

function constant(name: string, statements: string[]) {
  return `pub const ${name}: &[&str] = ${array(statements)};
`
}

function raw(text: string) {
  let marks = 1
  for (const match of text.matchAll(/"(#*)/g)) marks = Math.max(marks, match[1].length + 1)
  return `r${"#".repeat(marks)}"${text}"${"#".repeat(marks)}`
}

function header() {
  return "// Generated by script/gen/storage.ts from the pinned TypeScript. Do not edit."
}

function fail(message: string): never {
  console.error(message)
  process.exit(1)
}
