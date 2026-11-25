import { expect, test } from "bun:test"
import fs from "node:fs/promises"
import path from "node:path"
import { selectCapture, type Part } from "../lib/compare"

const packageRoot = path.resolve(import.meta.dir, "../..")
const runScript = path.join(import.meta.dir, "../run.ts")
const fakeServer = path.join(import.meta.dir, "../fixtures/fake-server.ts")
const infoScenario = path.join(import.meta.dir, "../scenarios/info-auth.scenario.ts")

const only = (...parts: Part[]) => new Set(parts)

const capture = {
  steps: [{ status: 200 }],
  events: [{ type: "server.connected" }],
  logs: { "<ses:1>": [] },
  model: [{ path: "/v1/chat/completions" }],
  database: { "opencode.db": { schema: [], tables: {} } },
  files: { "hello.txt": "hello\n" },
}

test("keeps only the selected capture parts", () => {
  expect(selectCapture(capture, only("steps", "database"))).toEqual({
    steps: [{ status: 200 }],
    database: { "opencode.db": { schema: [], tables: {} } },
  })
  expect(selectCapture(capture, only("files"))).toEqual({ files: { "hello.txt": "hello\n" } })
})

test("restricts each database entry to the requested tables", () => {
  const selected = selectCapture(
    {
      database: {
        "opencode.db": {
          schema: [
            { type: "table", name: "project", tbl_name: "project", sql: "create table project" },
            { type: "index", name: "project_id", tbl_name: "project", sql: "create index project_id" },
            { type: "table", name: "session", tbl_name: "session", sql: "create table session" },
            { type: "index", name: "session_id", tbl_name: "session", sql: "create index session_id" },
          ],
          tables: { project: [{ id: 1 }], session: [{ id: 2 }] },
        },
      },
    },
    only("database"),
    ["project"],
  )
  expect(selected).toEqual({
    database: {
      "opencode.db": {
        schema: [
          { type: "table", name: "project", tbl_name: "project", sql: "create table project" },
          { type: "index", name: "project_id", tbl_name: "project", sql: "create index project_id" },
        ],
        tables: { project: [{ id: 1 }] },
      },
    },
  })
})

test("drops schema rows whose table is not requested from every database entry", () => {
  const selected = selectCapture(
    {
      database: {
        "empty.db": {
          schema: [{ type: "index", name: "other_index", tbl_name: "other", sql: null }],
          tables: { other: [] },
        },
        "project.db": {
          schema: [{ type: "table", name: "project", tbl_name: "project", sql: null }],
          tables: { project: [] },
        },
      },
    },
    only("database"),
    ["project"],
  )
  expect(selected).toEqual({
    database: {
      "empty.db": { schema: [], tables: {} },
      "project.db": {
        schema: [{ type: "table", name: "project", tbl_name: "project", sql: null }],
        tables: { project: [] },
      },
    },
  })
})

test("leaves the database untouched without a table list", () => {
  const database = { "opencode.db": { schema: [], tables: { project: [] } } }
  expect(selectCapture({ database }, only("database"))).toEqual({ database })
})

async function runParity(outDir: string, args: string[], env: Record<string, string> = {}) {
  const child = Bun.spawn(["bun", runScript, ...args], {
    cwd: packageRoot,
    env: { ...process.env, PARITY_OUT_DIR: outDir, ...env },
    stdout: "pipe",
    stderr: "pipe",
  })
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
    child.exited,
  ])
  return { exitCode, output: stdout + stderr }
}

async function withTempDir(prefix: string, run: (directory: string) => Promise<void>) {
  await fs.mkdir("/tmp/opencode", { recursive: true })
  const directory = await fs.mkdtemp(path.join("/tmp/opencode", prefix))
  try {
    await run(directory)
  } finally {
    await fs.rm(directory, { recursive: true, force: true })
  }
}

function scenarioSource(steps: string, env: Record<string, string> = {}) {
  const scenarioModule = path.join(import.meta.dir, "../lib/scenario")
  return `import { defineScenario } from ${JSON.stringify(scenarioModule)}

export default defineScenario({
  env: ${JSON.stringify(env)},
  steps: [${steps}],
})
`
}

test("compares only the selected parts when the candidate rejects the event feed", async () => {
  await withTempDir("parity-only-", async (directory) => {
    const scenario = path.join(directory, "reject-events.scenario.ts")
    const source = scenarioSource('{ http: { method: "GET", path: "/api/info" } }', {
      FAKE_REJECT_EVENTS: "1",
    })
    await Bun.write(scenario, source)
    const result = await runParity(path.join(directory, "out"), [
      "--reference",
      fakeServer,
      "--candidate",
      fakeServer,
      "--only",
      "steps,database",
      scenario,
    ])
    expect(result.exitCode).toBe(0)
    expect(result.output).toContain("PASS reject-events")
    expect(result.output).toContain("events not compared")
    expect(result.output).toContain("websockets not compared")
  })
}, 60_000)

test("opens the event feed in default mode and fails when it is rejected", async () => {
  await withTempDir("parity-only-", async (directory) => {
    const scenario = path.join(directory, "reject-events.scenario.ts")
    const source = scenarioSource('{ http: { method: "GET", path: "/api/info" } }', {
      FAKE_REJECT_EVENTS: "1",
    })
    await Bun.write(scenario, source)
    const result = await runParity(path.join(directory, "out"), [
      "--reference",
      fakeServer,
      "--candidate",
      fakeServer,
      scenario,
    ])
    expect(result.exitCode).toBe(2)
    expect(result.output).toContain("event stream failed")
  })
}, 60_000)

test("ignores a step difference when only the database is compared", async () => {
  await withTempDir("parity-only-", async (outDir) => {
    const result = await runParity(
      outDir,
      ["--reference", fakeServer, "--candidate", fakeServer, "--only", "database", infoScenario],
      { PARITY_CANDIDATE_ENV: JSON.stringify({ FAKE_VERSION: "other" }) },
    )
    expect(result.exitCode).toBe(0)
    expect(result.output).toContain("PASS info-auth")
  })
}, 60_000)

test("reports a step difference when only steps are compared", async () => {
  await withTempDir("parity-only-", async (outDir) => {
    const result = await runParity(
      outDir,
      ["--reference", fakeServer, "--candidate", fakeServer, "--only", "steps", infoScenario],
      { PARITY_CANDIDATE_ENV: JSON.stringify({ FAKE_VERSION: "other" }) },
    )
    expect(result.exitCode).toBe(1)
    expect(result.output).toContain("steps/0/body/version")
  })
}, 60_000)

test("fails when a requested table matches no captured table", async () => {
  await withTempDir("parity-only-", async (outDir) => {
    const result = await runParity(outDir, [
      "--reference",
      fakeServer,
      "--candidate",
      fakeServer,
      "--only",
      "database",
      "--tables",
      "project",
      infoScenario,
    ])
    expect(result.exitCode).toBe(2)
    expect(result.output).toContain("--tables project matched no table")
  })
}, 60_000)

test("refuses a waitEvent scenario when events are not compared", async () => {
  await withTempDir("parity-only-", async (directory) => {
    const scenario = path.join(directory, "needs-events.scenario.ts")
    await Bun.write(scenario, scenarioSource('{ waitEvent: { type: "server.connected" } }'))
    const result = await runParity(path.join(directory, "out"), [
      "--reference",
      fakeServer,
      "--candidate",
      fakeServer,
      "--only",
      "steps",
      scenario,
    ])
    expect(result.exitCode).toBe(2)
    expect(result.output).toContain("scenario needs events")
  })
}, 60_000)

test("rejects invalid part and table selections", async () => {
  await withTempDir("parity-only-", async (outDir) => {
    const parts = await runParity(outDir, [
      "--reference",
      fakeServer,
      "--candidate",
      fakeServer,
      "--only",
      "steps,bogus",
      infoScenario,
    ])
    expect(parts.exitCode).toBe(2)
    expect(parts.output).toContain(
      "--only must list parts from steps, events, database, files, logs, model, got steps,bogus",
    )
    const tables = await runParity(outDir, [
      "--reference",
      fakeServer,
      "--candidate",
      fakeServer,
      "--only",
      "steps",
      "--tables",
      "project",
      infoScenario,
    ])
    expect(tables.exitCode).toBe(2)
    expect(tables.output).toContain("--tables requires database in --only")
    const logs = await runParity(outDir, [
      "--reference",
      fakeServer,
      "--candidate",
      fakeServer,
      "--only",
      "logs",
      infoScenario,
    ])
    expect(logs.exitCode).toBe(2)
    expect(logs.output).toContain("--only logs requires events")
  })
}, 60_000)
