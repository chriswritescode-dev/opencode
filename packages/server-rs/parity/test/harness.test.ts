import { expect, test } from "bun:test"
import path from "node:path"
import fs from "node:fs/promises"
import { Database } from "bun:sqlite"
import { dumpDatabases, listFiles } from "../lib/capture"
import { compareCaptures } from "../lib/compare"
import { parseSseFrames } from "../lib/drive"
import { normalizeCapture } from "../lib/normalize"
import { sandboxEnv } from "../lib/sandbox"

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
    {
      millis: context.start + 1_000,
      digits: String(context.start + 1_000),
      iso: "2023-11-14T22:13:21.000Z",
      outside: context.start - 10_000_000,
    },
    context,
  )
  expect(capture).toEqual({
    millis: "<time>",
    digits: "<time:digits>",
    iso: "<time:iso>",
    outside: context.start - 10_000_000,
  })
})

test("keeps an ISO timestamp distinct from a millisecond timestamp", () => {
  expect(normalizeCapture({ value: "2023-11-14T22:13:21.000Z" }, context)).toEqual({ value: "<time:iso>" })
  expect(normalizeCapture({ value: context.start + 1_000 }, context)).toEqual({ value: "<time>" })
})

test("replaces only numeric pids and string hostnames", () => {
  const capture = normalizeCapture(
    {
      running: { pid: 4_242, hostname: "build-box" },
      stopped: { pid: null, hostname: 7 },
    },
    context,
  )
  expect(capture).toEqual({
    running: { pid: "<pid>", hostname: "<hostname>" },
    stopped: { pid: null, hostname: 7 },
  })
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

test(
  "passes identical servers",
  async () => {
    await withTempDir("parity-out-", async (outDir) => {
      const result = await runParity(outDir, ["--reference", fakeServer, "--candidate", fakeServer, infoScenario])
      expect(result.exitCode).toBe(0)
      expect(result.output).toContain("PASS info-auth")
    })
  },
  60_000,
)

test(
  "reports a difference in the candidate environment",
  async () => {
    await withTempDir("parity-out-", async (outDir) => {
      const result = await runParity(
        outDir,
        ["--reference", fakeServer, "--candidate", fakeServer, infoScenario],
        { PARITY_CANDIDATE_ENV: JSON.stringify({ FAKE_VERSION: "other" }) },
      )
      expect(result.exitCode).toBe(1)
      expect(result.output).toContain("steps/0/body/version")
    })
  },
  60_000,
)

test(
  "reports a server that does not start",
  async () => {
    await withTempDir("parity-out-", async (outDir) => {
      const result = await runParity(outDir, ["--reference", fakeServer, "--candidate", exitTwo, infoScenario])
      expect(result.exitCode).toBe(2)
      expect(result.output).toContain("did not start")
    })
  },
  60_000,
)

test(
  "ignores events that arrive while the warm-up runs",
  async () => {
    await withTempDir("parity-out-", async (outDir) => {
      const result = await runParity(
        outDir,
        ["--reference", fakeServer, "--candidate", fakeServer, infoScenario],
        { PARITY_CANDIDATE_ENV: JSON.stringify({ FAKE_WARM_DELAY: "2500" }) },
      )
      expect(result.exitCode).toBe(0)
    })
  },
  60_000,
)

test(
  "passes a WebSocket echo scenario",
  async () => {
    await withTempDir("parity-out-", async (outDir) => {
      await withTempRoot(async (root) => {
        const scenario = path.join(root, "ws-echo-parity.scenario.ts")
        const scenarioModule = path.join(import.meta.dir, "../lib/scenario")
        const source = `import { defineScenario } from ${JSON.stringify(scenarioModule)}

export default defineScenario({
  steps: [{ ws: { path: "/echo", send: ["first", "second"], receive: 2 } }],
})
`
        await Bun.write(scenario, source)
        const result = await runParity(outDir, ["--reference", fakeServer, "--candidate", fakeServer, scenario])
        expect(result.exitCode).toBe(0)
        expect(result.output).toContain("PASS ws-echo-parity")
        const capture = (await Bun.file(path.join(outDir, "ws-echo-parity/reference.json")).json()) as {
          steps: { opened: boolean; frames: string[]; close: { code: number } }[]
        }
        expect(capture.steps[0].opened).toBe(true)
        expect(capture.steps[0].frames).toEqual(["first", "second"])
        expect(capture.steps[0].close.code).toBe(1000)
      })
    })
  },
  60_000,
)

test(
  "settles the event stream without adding a captured step",
  async () => {
    await withTempDir("parity-out-", async (outDir) => {
      await withTempRoot(async (root) => {
        const scenario = path.join(root, "settle-parity.scenario.ts")
        const scenarioModule = path.join(import.meta.dir, "../lib/scenario")
        const source = `import { defineScenario } from ${JSON.stringify(scenarioModule)}

export default defineScenario({
  steps: [{ http: { method: "GET", path: "/api/info" } }, { settle: true }],
})
`
        await Bun.write(scenario, source)
        const partial = await runParity(outDir, [
          "--reference",
          fakeServer,
          "--candidate",
          fakeServer,
          "--only",
          "steps",
          scenario,
        ])
        expect(partial.exitCode).toBe(0)
        expect(partial.output).toContain("PASS settle-parity")
        const full = await runParity(outDir, ["--reference", fakeServer, "--candidate", fakeServer, scenario])
        expect(full.exitCode).toBe(0)
        expect(full.output).toContain("PASS settle-parity")
      })
    })
  },
  60_000,
)

test(
  "templates the scripted endpoint into step bodies",
  async () => {
    await withTempDir("parity-out-", async (outDir) => {
      await withTempRoot(async (root) => {
        const scenario = path.join(root, "endpoint-parity.scenario.ts")
        const scenarioModule = path.join(import.meta.dir, "../lib/scenario")
        const source = `import { defineScenario } from ${JSON.stringify(scenarioModule)}

export default defineScenario({
  steps: [{
    http: {
      method: "POST",
      path: "/api/echo",
      body: { endpoint: "{{endpoint}}", server: "{{endpoint}}/console" },
    },
  }],
})
`
        await Bun.write(scenario, source)
        const result = await runParity(outDir, [
          "--reference",
          fakeServer,
          "--candidate",
          fakeServer,
          "--only",
          "steps",
          scenario,
        ])
        expect(result.exitCode).toBe(0)
        expect(result.output).toContain("PASS endpoint-parity")
        const capture = (await Bun.file(path.join(outDir, "endpoint-parity/reference.json")).json()) as {
          steps: { body: unknown }[]
        }
        expect(capture.steps[0].body).toEqual({
          endpoint: "http://<host>:<port>",
          server: "http://<host>:<port>/console",
        })
      })
    })
  },
  60_000,
)

test(
  "templates scenario env values",
  async () => {
    await withTempDir("parity-out-", async (outDir) => {
      await withTempRoot(async (root) => {
        const scenario = await writeScenario(
          root,
          "env-template",
          `{
  env: { FAKE_ECHO: "{{root}}/echo" },
  steps: [{ http: { method: "GET", path: "/api/env?name=FAKE_ECHO" } }],
}`,
        )
        const result = await runParity(outDir, ["--reference", fakeServer, "--candidate", fakeServer, scenario])
        expect(result.exitCode).toBe(0)
        const capture = (await Bun.file(path.join(outDir, "env-template/reference.json")).json()) as {
          steps: { body: { value: string } }[]
        }
        expect(capture.steps[0].body.value.endsWith("<root>/echo")).toBe(true)
      })
    })
  },
  60_000,
)

test(
  "creates a git repository whose metadata is not captured",
  async () => {
    await withTempDir("parity-out-", async (outDir) => {
      await withTempRoot(async (root) => {
        const scenario = await writeScenario(
          root,
          "git-repository",
          `{
  files: { "a.txt": "a\\n" },
  steps: [
    { git: { args: ["init", "-q"] } },
    { git: { args: ["add", "."] } },
    { git: { args: ["commit", "-qm", "one"] } },
    { http: { method: "GET", path: "/api/config" } },
  ],
}`,
        )
        const result = await runParity(outDir, ["--reference", fakeServer, "--candidate", fakeServer, scenario])
        expect(result.exitCode).toBe(0)
        expect(result.output).toContain("PASS git-repository")
        const capture = (await Bun.file(path.join(outDir, "git-repository/reference.json")).json()) as {
          files: Record<string, string>
        }
        expect(capture.files["work/a.txt"]).toBe("a\n")
        expect(Object.keys(capture.files).some((file) => file.includes(".git"))).toBe(false)
      })
    })
  },
  60_000,
)

test(
  "stops the scenario when a git step fails",
  async () => {
    await withTempDir("parity-out-", async (outDir) => {
      await withTempRoot(async (root) => {
        const scenario = await writeScenario(
          root,
          "git-failure",
          `{ steps: [{ git: { args: ["rev-parse", "--verify", "missing"] } }] }`,
        )
        const result = await runParity(outDir, ["--reference", fakeServer, "--candidate", fakeServer, scenario])
        expect(result.exitCode).toBe(2)
        expect(result.output).toContain("git rev-parse")
      })
    })
  },
  60_000,
)

test(
  "rejects a git cwd outside the work directory",
  async () => {
    await withTempDir("parity-out-", async (outDir) => {
      await withTempRoot(async (root) => {
        const scenario = await writeScenario(
          root,
          "git-outside",
          `{ steps: [{ git: { args: ["status"], cwd: "../outside" } }] }`,
        )
        const result = await runParity(outDir, ["--reference", fakeServer, "--candidate", fakeServer, scenario])
        expect(result.exitCode).toBe(2)
        expect(result.output).toContain("cwd is outside")
      })
    })
  },
  60_000,
)

test(
  "runs setup git commands before the server starts",
  async () => {
    await withTempDir("parity-out-", async (outDir) => {
      await withTempRoot(async (root) => {
        const scenario = await writeScenario(
          root,
          "setup-git",
          `{
  setup: [
    { files: { "a.txt": "a\\n" } },
    { git: { args: ["init", "-q"] } },
    { git: { args: ["add", "."] } },
    { git: { args: ["commit", "-qm", "one"] } },
  ],
  steps: [{ http: { method: "GET", path: "/api/config" } }],
}`,
        )
        const result = await runParity(outDir, ["--reference", fakeServer, "--candidate", fakeServer, scenario])
        expect(result.exitCode).toBe(0)
        expect(result.output).toContain("PASS setup-git")
        const capture = (await Bun.file(path.join(outDir, "setup-git/reference.json")).json()) as {
          files: Record<string, string>
        }
        expect(capture.files["work/a.txt"]).toBe("a\n")
        expect(Object.keys(capture.files).some((file) => file.includes(".git"))).toBe(false)
      })
    })
  },
  60_000,
)

test(
  "stops the scenario when a setup git step fails",
  async () => {
    await withTempDir("parity-out-", async (outDir) => {
      await withTempRoot(async (root) => {
        const scenario = await writeScenario(
          root,
          "setup-git-failure",
          `{
  setup: [{ git: { args: ["rev-parse", "--verify", "missing"] } }],
  steps: [{ http: { method: "GET", path: "/api/config" } }],
}`,
        )
        const result = await runParity(outDir, ["--reference", fakeServer, "--candidate", fakeServer, scenario])
        expect(result.exitCode).toBe(2)
        expect(result.output).toContain("git rev-parse")
      })
    })
  },
  60_000,
)

async function withTempDir(prefix: string, run: (directory: string) => Promise<void>) {
  await fs.mkdir("/tmp/opencode", { recursive: true })
  const directory = await fs.mkdtemp(path.join("/tmp/opencode", prefix))
  try {
    await run(directory)
  } finally {
    await fs.rm(directory, { recursive: true, force: true })
  }
}

async function withTempRoot(run: (root: string) => Promise<void>) {
  await withTempDir("parity-capture-", run)
}

async function writeScenario(root: string, name: string, source: string) {
  const scenario = path.join(root, `${name}.scenario.ts`)
  const scenarioModule = path.join(import.meta.dir, "../lib/scenario")
  await Bun.write(
    scenario,
    `import { defineScenario } from ${JSON.stringify(scenarioModule)}\n\nexport default defineScenario(${source})\n`,
  )
  return scenario
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

test("writes a scenario models value to the sandbox catalog", async () => {
  await withTempRoot(async (root) => {
    const models = { providers: [{ id: "parity", models: [{ id: "test-model" }] }] }
    await sandboxEnv(root, {}, {}, "http://127.0.0.1:1", models)
    expect(await Bun.file(path.join(root, "models.json")).text()).toBe(JSON.stringify(models))
    await sandboxEnv(root, {}, {}, "http://127.0.0.1:1")
    expect(await Bun.file(path.join(root, "models.json")).text()).toBe("{}")
  })
})

test("skips git metadata when listing files", async () => {
  await withTempRoot(async (root) => {
    await Bun.write(path.join(root, "file.txt"), "text")
    await fs.mkdir(path.join(root, ".git"), { recursive: true })
    await Bun.write(path.join(root, ".git/config"), "[core]\n")
    await fs.mkdir(path.join(root, "work/.git"), { recursive: true })
    await Bun.write(path.join(root, "work/.git/HEAD"), "ref: refs/heads/main\n")
    expect(await listFiles(root)).toEqual({ "file.txt": "text" })
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

test("numbers ids independently of object key order", () => {
  const first = normalizeCapture(
    { steps: [{ id: "ses_0123456789abcdefghijklmnop", parentID: "ses_ponmlkjihgfedcba9876543210" }] },
    context,
  )
  const second = normalizeCapture(
    { steps: [{ parentID: "ses_ponmlkjihgfedcba9876543210", id: "ses_0123456789abcdefghijklmnop" }] },
    context,
  )
  expect(first).toEqual({ steps: [{ id: "<ses:1>", parentID: "<ses:2>" }] })
  expect(second).toEqual(first)
})

test("preserves the formatting of JSON strings that round-trip exactly", () => {
  const value = { createdAt: context.start + 1_000, id: "ses_0123456789abcdefghijklmnop" }
  const compact = JSON.stringify(value)
  const pretty = JSON.stringify(value, null, 2) + "\n"
  const fourSpaces = JSON.stringify(value, null, 4)
  const normalizedCompact = normalizeCapture(compact, context)
  const normalizedPretty = normalizeCapture(pretty, context)
  expect(normalizedCompact).toBe('{"createdAt":"<time>","id":"<ses:1>"}')
  expect(normalizedPretty).toBe(JSON.stringify({ createdAt: "<time>", id: "<ses:1>" }, null, 2) + "\n")
  expect(normalizeCapture(fourSpaces, context)).toBe(JSON.stringify({ createdAt: "<time>", id: "<ses:1>" }, null, 4))
  expect(normalizedCompact).not.toBe(normalizedPretty)
})

test("normalizes the same pretty JSON with different ids equally", () => {
  const first = JSON.stringify({ id: "ses_0123456789abcdefghijklmnop" }, null, 2) + "\n"
  const second = JSON.stringify({ id: "ses_ponmlkjihgfedcba9876543210" }, null, 2) + "\n"
  expect(normalizeCapture(first, context)).toBe(normalizeCapture(second, context))
})

test("applies string rules to JSON text that does not round-trip exactly", () => {
  expect(normalizeCapture('{"id":"ses_0123456789abcdefghijklmnop","value":1.0}', context)).toBe(
    '{"id":"<ses:1>","value":1.0}',
  )
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
