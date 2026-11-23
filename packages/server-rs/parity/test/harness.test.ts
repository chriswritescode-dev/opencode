import { expect, test } from "bun:test"
import path from "node:path"
import { compareCaptures } from "../lib/compare"
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
