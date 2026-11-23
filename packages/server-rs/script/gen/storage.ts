import { mkdir } from "node:fs/promises"
import { loadMigrations, type Transaction } from "../storage/typescript"

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
for (const id of HAND_PORTED)
  if (!registry.has(id)) fail(`storage: ${id} is missing from the migration registry`)

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

console.log(
  `storage: ${migrations.length} migrations, ${HAND_PORTED.size} hand-ported, ${schemaStatements.length} schema statements`,
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
