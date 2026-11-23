import { Database as BunDatabase } from "bun:sqlite"
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { dump, openTypescript } from "./storage/typescript"

type Dump = {
  schema: unknown[]
  tables: Record<string, { columns: string[]; rows: unknown[][] }>
  foreignKeyViolations: number
}

type Corpus = {
  masks: Record<string, string[]>
  fixtures: Array<{ name: string; authJson: string | null; start: string[] }>
}

const root = new URL("..", import.meta.url).pathname
const binary = `${root}target/debug/examples/storage`
const corpus = (await Bun.file(`${root}crates/storage/tests/corpus/migrations.json`).json()) as Corpus

const build = Bun.spawnSync(
  ["bun", "run", "cargo", "build", "--locked", "-q", "-p", "opencode-storage", "--example", "storage"],
  { cwd: root },
)
if (build.exitCode !== 0) {
  process.stderr.write(build.stderr)
  process.exit(1)
}

const directory = await mkdtemp(join(tmpdir(), "opencode-interop-"))
try {
  const rust = join(directory, "rust", "opencode.db")
  const rustData = join(directory, "rust")
  await mkdir(rustData, { recursive: true })
  run("open", rust, rustData)
  const rustBefore = rustDump(rust)
  await openTypescript(rust, rustData)
  compare("typescript opens the rust database unchanged", rustBefore, rustDump(rust))

  const typescript = join(directory, "typescript", "opencode.db")
  const typescriptData = join(directory, "typescript")
  await mkdir(typescriptData, { recursive: true })
  await openTypescript(typescript, typescriptData)
  const typescriptBefore = databaseDump(typescript, {})
  run("open", typescript, typescriptData)
  compare("rust opens the typescript database unchanged", typescriptBefore, databaseDump(typescript, {}))

  compare(
    "the rust and typescript databases match",
    databaseDump(rust, corpus.masks),
    databaseDump(typescript, corpus.masks),
  )

  const fixture = corpus.fixtures.find((item) => item.name === "before-v2-split")
  if (fixture === undefined) fail("interop: the corpus has no before-v2-split fixture")
  const rustFixture = join(directory, "fixture-rust", "opencode.db")
  const rustFixtureData = join(directory, "fixture-rust")
  const typescriptFixture = join(directory, "fixture-typescript", "opencode.db")
  const typescriptFixtureData = join(directory, "fixture-typescript")
  await mkdir(rustFixtureData, { recursive: true })
  await mkdir(typescriptFixtureData, { recursive: true })
  seed(rustFixture, fixture.start)
  seed(typescriptFixture, fixture.start)
  if (fixture.authJson !== null) {
    await writeFile(join(rustFixtureData, "auth.json"), fixture.authJson)
    await writeFile(join(typescriptFixtureData, "auth.json"), fixture.authJson)
  }
  run("open", rustFixture, rustFixtureData)
  await openTypescript(typescriptFixture, typescriptFixtureData)
  compare(
    "the before-v2-split fixture matches",
    databaseDump(rustFixture, corpus.masks),
    databaseDump(typescriptFixture, corpus.masks),
  )
} finally {
  await rm(directory, { recursive: true, force: true })
}

function run(command: string, file: string, data?: string) {
  const args = data === undefined ? [binary, command, file] : [binary, command, file, "--data", data]
  const result = Bun.spawnSync(args, { cwd: root })
  if (result.exitCode !== 0) fail(`interop: storage ${command} failed: ${result.stderr.toString().trim()}`)
  return result.stdout.toString()
}

function rustDump(file: string) {
  return JSON.parse(run("dump", file)) as Dump
}

function databaseDump(path: string, masks: Record<string, string[]>): Dump {
  const db = new BunDatabase(path)
  try {
    return dump(db, masks) as Dump
  } finally {
    db.close()
  }
}

function seed(path: string, statements: string[]) {
  const db = new BunDatabase(path, { create: true })
  try {
    for (const statement of statements) db.run(statement)
  } finally {
    db.close()
  }
}

function compare(name: string, expected: Dump, actual: Dump) {
  if (!same(expected.schema, actual.schema)) difference(name, "the schema differs", expected.schema, actual.schema)
  if (expected.foreignKeyViolations !== actual.foreignKeyViolations)
    difference(name, "the foreign key violations differ", expected.foreignKeyViolations, actual.foreignKeyViolations)
  for (const table of Array.from(new Set([...Object.keys(expected.tables), ...Object.keys(actual.tables)])).sort()) {
    const left = expected.tables[table]
    const right = actual.tables[table]
    if (left === undefined || right === undefined) difference(name, `table ${table} differs`, left, right)
    if (!same(left.columns, right.columns))
      difference(name, `table ${table} columns differ`, left.columns, right.columns)
    if (!same(left.rows, right.rows)) difference(name, `table ${table} rows differ`, left.rows, right.rows)
  }
  console.log(`interop: ${name}`)
}

function same(left: unknown, right: unknown) {
  return JSON.stringify(left) === JSON.stringify(right)
}

function difference(name: string, part: string, expected: unknown, actual: unknown): never {
  console.error(`interop: ${name}: ${part}`)
  for (const value of [expected, actual]) {
    const text = JSON.stringify(value)
    if (text !== undefined && text.length <= 400) console.error(text)
  }
  process.exit(1)
}

function fail(message: string): never {
  console.error(message)
  process.exit(1)
}
