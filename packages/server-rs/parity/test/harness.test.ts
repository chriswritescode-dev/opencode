import { expect, test } from "bun:test"
import path from "node:path"
import fs from "node:fs/promises"
import { Database } from "bun:sqlite"
import { dumpDatabases, listFiles } from "../lib/capture"
import { compareCaptures } from "../lib/compare"
import { parseSseFrames } from "../lib/drive"
import { normalizeCapture } from "../lib/normalize"

const root = "/tmp/opencode-parity-test"
const context = { root, start: 1_700_000_000_000, end: 1_700_000_100_000 }
const packageRoot = path.resolve(import.meta.dir, "../..")
const runScript = path.join(import.meta.dir, "../run.ts")
const fakeServer = path.join(import.meta.dir, "../fixtures/fake-server.ts")
const exitTwo = path.join(import.meta.dir, "../fixtures/exit-two.sh")
const infoScenario = path.join(import.meta.dir, "../scenarios/info-auth.scenario.ts")

test("normalizes equal ids to one placeholder and distinct ids apart", () => {
  const capture = normalizeCapture(
    {
      first: "ses_0123456789abcdefghijklmnop",
      second: "ses_0123456789abcdefghijklmnop",
      third: "msg_0123456789abcdefghijklmnop",
    },
    context,
  )
  expect(capture).toEqual({ first: "<ses:1>", second: "<ses:1>", third: "<msg:1>" })
})

test("normalizes millisecond and ISO-8601 timestamps in the capture window", () => {
  const capture = normalizeCapture(
    { millis: context.start + 1_000, iso: "2023-11-14T22:13:21.000Z", outside: context.start - 10_000_000 },
    context,
  )
  expect(capture).toEqual({ millis: "<time>", iso: "<time>", outside: context.start - 10_000_000 })
})

test("normalizes the sandbox root and loopback hosts", () => {
  const capture = normalizeCapture(
    {
      path: `${root}/work/session.json`,
      encoded: `${encodeURIComponent(root)}/work`,
      ip: "http://127.0.0.1:4321/api/info",
      host: "http://localhost:4321/api/info",
    },
    context,
  )
  expect(capture).toEqual({
    path: "<root>/work/session.json",
    encoded: "<root>/work",
    ip: "http://<host>:<port>/api/info",
    host: "http://<host>:<port>/api/info",
  })
})

test("compares nested paths", () => {
  const differences = compareCaptures(
    { steps: [{ body: { x: 1 } }] },
    { steps: [{ body: { x: 2 } }] },
  )
  expect(differences).toEqual([{ path: "steps/0/body/x", reference: 1, candidate: 2 }])
})

test("compares missing array elements", () => {
  const differences = compareCaptures(
    { steps: [{ body: { x: [1, 2] } }] },
    { steps: [{ body: { x: [1] } }] },
  )
  expect(differences).toEqual([{ path: "steps/0/body/x/1", reference: 2, candidate: undefined }])
})

async function runParity(args: string[], env: Record<string, string> = {}) {
  const child = Bun.spawn(["bun", runScript, ...args], {
    cwd: packageRoot,
    env: { ...process.env, ...env },
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

test(
  "passes identical servers",
  async () => {
    const result = await runParity(["--reference", fakeServer, "--candidate", fakeServer, infoScenario])
    expect(result.exitCode).toBe(0)
    expect(result.output).toContain("PASS info-auth")
  },
  60_000,
)

test(
  "reports a difference in the candidate environment",
  async () => {
    const result = await runParity(["--reference", fakeServer, "--candidate", fakeServer, infoScenario], {
      PARITY_CANDIDATE_ENV: JSON.stringify({ FAKE_VERSION: "other" }),
    })
    expect(result.exitCode).toBe(1)
    expect(result.output).toContain("steps/0/body/version")
  },
  60_000,
)

test(
  "reports a server that does not start",
  async () => {
    const result = await runParity(["--reference", fakeServer, "--candidate", exitTwo, infoScenario])
    expect(result.exitCode).toBe(2)
    expect(result.output).toContain("did not start")
  },
  60_000,
)

test(
  "ignores events that arrive while the warm-up runs",
  async () => {
    const result = await runParity(["--reference", fakeServer, "--candidate", fakeServer, infoScenario], {
      PARITY_CANDIDATE_ENV: JSON.stringify({ FAKE_WARM_DELAY: "2500" }),
    })
    expect(result.exitCode).toBe(0)
  },
  60_000,
)

async function withTempRoot(run: (root: string) => Promise<void>) {
  await fs.mkdir("/tmp/opencode", { recursive: true })
  const root = await fs.mkdtemp(path.join("/tmp/opencode", "parity-capture-"))
  try {
    await run(root)
  } finally {
    await fs.rm(root, { recursive: true, force: true })
  }
}

test("parses SSE frames and ignores heartbeat comments", () => {
  const text =
    ": heartbeat\n\ndata: {\"id\":\"evt_a\",\"type\":\"server.connected\",\"data\":{}}\n\ndata: {\"type\":\"session.created\",\"data\":{\"sessionID\":\"ses_one\"}}\n\n"
  expect(parseSseFrames(text)).toEqual([
    { id: "evt_a", type: "server.connected", data: {} },
    { type: "session.created", data: { sessionID: "ses_one" } },
  ])
  expect(parseSseFrames(": heartbeat\n\n")).toEqual([])
})

test("dumps SQLite schema, tables and blobs", async () => {
  await withTempRoot(async (root) => {
    const database = new Database(path.join(root, "data.db"))
    database.run("create table alpha (id integer, payload blob)")
    database.run("create table beta (name text)")
    database.query("insert into alpha (id, payload) values (?, ?)").run(1, new Uint8Array([0, 255, 16]))
    database.query("insert into beta (name) values (?)").run("value")
    database.close()
    const databases = await dumpDatabases(root)
    expect(Object.keys(databases)).toEqual(["data.db"])
    expect(databases["data.db"].schema.map((entry) => `${entry.type}:${entry.name}`)).toEqual([
      "table:alpha",
      "table:beta",
    ])
    expect(databases["data.db"].tables.alpha).toEqual([{ id: 1, payload: "hex:00ff10" }])
    expect(databases["data.db"].tables.beta).toEqual([{ name: "value" }])
  })
})

test("lists text and hashed files but skips databases, siblings, models and logs", async () => {
  await withTempRoot(async (root) => {
    const database = new Database(path.join(root, "data.db"))
    database.run("create table data (id integer)")
    database.close()
    await Bun.write(path.join(root, "data.db-wal"), "wal")
    await Bun.write(path.join(root, "data.db-shm"), "shm")
    await Bun.write(path.join(root, "data.db-journal"), "journal")
    await Bun.write(path.join(root, "models.json"), "{}")
    await fs.mkdir(path.join(root, "home/.local/share/opencode/log"), { recursive: true })
    await Bun.write(path.join(root, "home/.local/share/opencode/log/opencode-local.log"), "log line")
    await Bun.write(path.join(root, "text.txt"), "hello")
    await Bun.write(path.join(root, "binary.bin"), new Uint8Array([255, 254, 0]))
    const files = await listFiles(root)
    expect(Object.keys(files).sort()).toEqual(["binary.bin", "text.txt"])
    expect(files["text.txt"]).toBe("hello")
    expect(files["binary.bin"]).toBe(
      "sha256:ba778c0261008c8f71ae4061ad0162ffcbe63b52c91f89f236738131d1217ec7",
    )
  })
})

test("numbers slugs per first appearance", () => {
  const capture = normalizeCapture(
    { first: { slug: "neon-planet" }, second: { slug: "swift-orchid" }, third: { slug: "neon-planet" } },
    context,
  )
  expect(capture).toEqual({
    first: { slug: "<slug:1>" },
    second: { slug: "<slug:2>" },
    third: { slug: "<slug:1>" },
  })
})

test("normalizes base64 JSON cursors and sorts database rows", () => {
  const cursor = Buffer.from(
    JSON.stringify({
      anchor: { id: "ses_0123456789abcdefghijklmnop", time: context.start + 1_000, direction: "next" },
    }),
  ).toString("base64")
  const capture = normalizeCapture(
    {
      cursor,
      logs: { ses_0123456789abcdefghijklmnop: [] },
      database: { db: { schema: [], tables: { rows: [{ value: 2 }, { value: 1 }] } } },
    },
    context,
  ) as {
    cursor: string
    logs: Record<string, unknown>
    database: { db: { tables: { rows: unknown[] } } }
  }
  expect(JSON.parse(Buffer.from(capture.cursor, "base64").toString("utf-8"))).toEqual({
    anchor: { id: "<ses:1>", time: "<time>", direction: "next" },
  })
  expect(capture.logs).toEqual({ "<ses:1>": [] })
  expect(capture.database.db.tables.rows).toEqual([{ value: 1 }, { value: 2 }])
})
