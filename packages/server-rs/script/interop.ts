import { Database as BunDatabase } from "bun:sqlite"
import { access, mkdir, mkdtemp, readdir, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { dirname, join } from "node:path"
import { startRegistry } from "./npm/registry"
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

type Package = { directory: string; name: string; version: string }

const root = new URL("..", import.meta.url).pathname
class InteropDifference extends Error {}

const binary = `${root}target/debug/examples/storage`
const corpus = (await Bun.file(`${root}crates/storage/tests/corpus/migrations.json`).json()) as Corpus

const build = Bun.spawnSync(
  [
    "bun",
    "run",
    "cargo",
    "build",
    "--locked",
    "-q",
    "--target-dir",
    `${root}target`,
    "-p",
    "opencode-storage",
    "--example",
    "storage",
  ],
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
} catch (caught) {
  console.error(caught instanceof InteropDifference ? caught.message : caught)
  process.exitCode = 1
} finally {
  await rm(directory, { recursive: true, force: true })
}

const npmBinary = `${root}target/debug/examples/npm`
const npmBuild = Bun.spawnSync(
  [
    "bun",
    "run",
    "cargo",
    "build",
    "--locked",
    "-q",
    "--target-dir",
    `${root}target`,
    "-p",
    "opencode-plugin",
    "--example",
    "npm",
  ],
  { cwd: root },
)
if (npmBuild.exitCode !== 0) {
  process.stderr.write(npmBuild.stderr)
  process.exit(1)
}

const npmDirectory = await mkdtemp(join(tmpdir(), "opencode-npm-interop-"))
const npmHome = join(npmDirectory, "home")
await mkdir(npmHome, { recursive: true })
const registry = await startRegistry({
  "interop-fixture": { "1.0.0": { "index.js": "module.exports = 1\n" } },
})
const npmEnv = { ...process.env, npm_config_registry: registry.url, HOME: npmHome }
try {
  const rustShared = "rust and typescript share one npm cache"
  const cacheA = join(npmDirectory, "a", "cache")
  const stateA = join(npmDirectory, "a", "state")
  const rustAdded = await npmRust<Package>(["add", "interop-fixture@^1", "--cache", cacheA, "--state", stateA])
  const typescriptResolved = await npmTypescript<Package>([
    "resolve",
    "interop-fixture@^1",
    "--cache",
    cacheA,
    "--state",
    stateA,
  ])
  expectSamePackage(rustShared, rustAdded, typescriptResolved)
  const generations = await generationCount(rustAdded.directory)
  const typescriptAdded = await npmTypescript<Package>([
    "add",
    "interop-fixture@^1",
    "--cache",
    cacheA,
    "--state",
    stateA,
  ])
  if (typescriptAdded.directory !== rustAdded.directory)
    difference(rustShared, "the package directories differ", rustAdded.directory, typescriptAdded.directory)
  const generationsAfterAdd = await generationCount(rustAdded.directory)
  if (generationsAfterAdd !== generations)
    difference(rustShared, "the generation count changed", generations, generationsAfterAdd)
  console.log(`interop: ${rustShared}`)

  const typescriptShared = "typescript and rust share one npm cache"
  const cacheB = join(npmDirectory, "b", "cache")
  const stateB = join(npmDirectory, "b", "state")
  const typescriptAddedFirst = await npmTypescript<Package>([
    "add",
    "interop-fixture@^1",
    "--cache",
    cacheB,
    "--state",
    stateB,
  ])
  const rustResolved = await npmRust<Package>([
    "resolve",
    "interop-fixture@^1",
    "--cache",
    cacheB,
    "--state",
    stateB,
  ])
  expectSamePackage(typescriptShared, typescriptAddedFirst, rustResolved)
  console.log(`interop: ${typescriptShared}`)

  const rejectedFallback = "a rejected newer version falls back to the previous generation"
  await registry.publish("interop-fixture", "1.1.0", { "index.js": "module.exports = 2\n" })
  const updated = await npmRust<Package>(["update", "interop-fixture@^1", "--cache", cacheA, "--state", stateA])
  if (updated.version !== "1.1.0") difference(rejectedFallback, "the updated version differs", "1.1.0", updated.version)
  const rejected = await npmRust<boolean>([
    "reject",
    "interop-fixture@^1",
    "1.1.0",
    "--cache",
    cacheA,
    "--state",
    stateA,
  ])
  if (rejected !== true) difference(rejectedFallback, "the rejection was not recorded", true, rejected)
  const fellBack = await npmTypescript<Package>([
    "resolve",
    "interop-fixture@^1",
    "--cache",
    cacheA,
    "--state",
    stateA,
  ])
  if (fellBack.version !== "1.0.0")
    difference(rejectedFallback, "the resolved version differs", "1.0.0", fellBack.version)
  const marker = `${generationRoot(updated.directory)}.rejected`
  const markerPresent = await access(marker).then(
    () => true,
    () => false,
  )
  if (!markerPresent) fail(`interop: ${rejectedFallback}: the marker ${marker} is missing`)
  console.log(`interop: ${rejectedFallback}`)
} catch (caught) {
  console.error(caught instanceof InteropDifference ? caught.message : caught)
  process.exitCode = 1
} finally {
  await registry.stop()
  await rm(npmDirectory, { recursive: true, force: true })
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
  const lines = [`interop: ${name}: ${part}`]
  for (const value of [expected, actual]) {
    const text = JSON.stringify(value)
    if (text !== undefined && text.length <= 400) lines.push(text)
  }
  fail(lines.join("\n"))
}

function fail(message: string): never {
  throw new InteropDifference(message)
}

async function npmRust<T>(args: string[]): Promise<T> {
  return spawnNpm<T>([npmBinary], "rust", args)
}

async function npmTypescript<T>(args: string[]): Promise<T> {
  return spawnNpm<T>(["bun", "run", join(root, "script/npm/typescript.ts")], "typescript", args)
}

async function spawnNpm<T>(command: string[], label: string, args: string[]): Promise<T> {
  const invocation = [...command, ...args]
  const subprocess = Bun.spawn(invocation, { cwd: root, env: npmEnv, stdout: "pipe", stderr: "pipe" })
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(subprocess.stdout).text(),
    new Response(subprocess.stderr).text(),
    subprocess.exited,
  ])
  if (exitCode !== 0) {
    const output = [stderr.trim(), stdout.trim()].filter((line) => line.length > 0).join("\n")
    fail(`interop: npm ${label} failed: ${invocation.join(" ")}\n${output}`)
  }
  return JSON.parse(stdout) as T
}

function generationRoot(directory: string) {
  return dirname(dirname(directory))
}

async function generationCount(directory: string) {
  const entries = await readdir(dirname(generationRoot(directory)))
  return entries.filter((entry) => /^\d+$/.test(entry)).length
}

function expectSamePackage(name: string, expected: Package, actual: Package) {
  if (expected.directory !== actual.directory)
    difference(name, "the package directories differ", expected.directory, actual.directory)
  if (expected.version !== actual.version)
    difference(name, "the package versions differ", expected.version, actual.version)
}
