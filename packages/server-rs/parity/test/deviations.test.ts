import { expect, test } from "bun:test"
import fs from "node:fs/promises"
import path from "node:path"
import { deviationWarnings, explain, loadDeviations, type Deviation } from "../lib/deviations"
import type { Difference } from "../lib/compare"

const packageRoot = path.resolve(import.meta.dir, "../..")
const runScript = path.join(import.meta.dir, "../run.ts")
const fakeServer = path.join(import.meta.dir, "../fixtures/fake-server.ts")
const infoScenario = path.join(import.meta.dir, "../scenarios/info-auth.scenario.ts")

async function withTempLedger(run: (directory: string) => Promise<void>) {
  await fs.mkdir("/tmp/opencode", { recursive: true })
  const root = await fs.mkdtemp(path.join("/tmp/opencode", "parity-deviations-"))
  try {
    await run(root)
  } finally {
    await fs.rm(root, { recursive: true, force: true })
  }
}

function ledgerText(accepted: string, block: boolean) {
  const lines = ["# OR-99: version differs", "", `Accepted: ${accepted}`]
  if (block) lines.push("", "```parity", "info-auth steps/*/body/version", "```")
  return lines.join("\n") + "\n"
}

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

test("loads deviations, ignoring the readme and deriving accepted and rules", async () => {
  await withTempLedger(async (directory) => {
    const write = (name: string, text: string) => Bun.write(path.join(directory, name), text)
    await write(
      "info-auth.md",
      "# info-auth\n\nAccepted: Test Owner, 2026-01-01\n\n```parity\ninfo-auth steps/*/body/version\n```\n",
    )
    await write(
      "pending.md",
      "# pending\n\nAccepted: pending owner\n\n```parity\ninfo-auth steps/*/body/version\n```\n",
    )
    await write("no-block.md", "# no-block\n\nAccepted: Test Owner, 2026-01-01\n")
    await write("missing.md", "# missing\n\nAccepted:\n")
    await write("README.md", "# Deviations\n")
    const deviations = await loadDeviations(directory)
    expect(deviations.map((deviation) => deviation.id)).toEqual(["info-auth", "missing", "no-block", "pending"])
    expect(deviations.map((deviation) => deviation.accepted)).toEqual([true, false, true, false])
    expect(deviations.map((deviation) => deviation.rules === undefined)).toEqual([false, true, true, false])
    expect(deviations[0].rules).toEqual([{ scenario: "info-auth", path: "steps/*/body/version" }])
  })
})

test("explains matching differences from accepted deviations only", () => {
  const difference = (differencePath: string): Difference => ({
    path: differencePath,
    reference: "a",
    candidate: "b",
  })
  const rules = [{ scenario: "info-auth", path: "steps/*/body/version" }]
  const accepted: Deviation = { id: "accepted", accepted: true, rules }
  const result = explain(
    [difference("steps/0/body/version"), difference("steps/1/status")],
    "info-auth",
    [accepted],
  )
  expect(result.explained.map((item) => item.difference.path)).toEqual(["steps/0/body/version"])
  expect(result.explained.map((item) => item.id)).toEqual(["accepted"])
  expect(result.unexplained.map((item) => item.path)).toEqual(["steps/1/status"])
  const pending = explain([difference("steps/0/body/version")], "info-auth", [
    { ...accepted, id: "pending", accepted: false },
  ])
  expect(pending.explained).toEqual([])
  expect(pending.unexplained.map((item) => item.path)).toEqual(["steps/0/body/version"])
})

test("warns about unaccepted deviations and accepted ones that matched nothing", () => {
  const warnings = deviationWarnings(
    [
      { id: "matched", accepted: true, rules: [{ scenario: "info-auth", path: "steps/*" }] },
      { id: "unmatched", accepted: true, rules: [{ scenario: "info-auth", path: "steps/*" }] },
      { id: "pending", accepted: false, rules: undefined },
    ],
    new Set(["matched"]),
  )
  expect(warnings).toEqual([
    "warning: deviation unmatched matched nothing",
    "warning: deviation pending is not accepted",
  ])
})

test(
  "explains differences accepted by the ledger",
  async () => {
    await withTempLedger(async (directory) => {
      await Bun.write(path.join(directory, "OR-99-version.md"), ledgerText("Test Owner, 2026-01-01", true))
      const result = await runParity(
        ["--reference", fakeServer, "--candidate", fakeServer, "--deviations", directory, infoScenario],
        { PARITY_CANDIDATE_ENV: JSON.stringify({ FAKE_VERSION: "other" }) },
      )
      expect(result.exitCode).toBe(0)
      expect(result.output).toContain("PASS info-auth")
      expect(result.output).toContain("explained by")
      expect(result.output).toContain("OR-99-version")
    })
  },
  60_000,
)

test(
  "reports differences without accepted deviations",
  async () => {
    await withTempLedger(async (directory) => {
      const result = await runParity(
        ["--reference", fakeServer, "--candidate", fakeServer, "--deviations", directory, infoScenario],
        { PARITY_CANDIDATE_ENV: JSON.stringify({ FAKE_VERSION: "other" }) },
      )
      expect(result.exitCode).toBe(1)
      expect(result.output).toContain("steps/0/body/version")
    })
  },
  60_000,
)

test(
  "does not explain differences with a pending acceptance",
  async () => {
    await withTempLedger(async (directory) => {
      await Bun.write(path.join(directory, "OR-99-version.md"), ledgerText("pending owner", true))
      const result = await runParity(
        ["--reference", fakeServer, "--candidate", fakeServer, "--deviations", directory, infoScenario],
        { PARITY_CANDIDATE_ENV: JSON.stringify({ FAKE_VERSION: "other" }) },
      )
      expect(result.exitCode).toBe(1)
    })
  },
  60_000,
)

test(
  "rejects an accepted deviation without a parity block before running scenarios",
  async () => {
    await withTempLedger(async (directory) => {
      await Bun.write(path.join(directory, "OR-99-version.md"), ledgerText("Test Owner, 2026-01-01", false))
      const result = await runParity([
        "--reference",
        fakeServer,
        "--candidate",
        fakeServer,
        "--deviations",
        directory,
        infoScenario,
      ])
      expect(result.exitCode).toBe(2)
      expect(result.output).toContain("deviation OR-99-version has no parity block")
    })
  },
  60_000,
)

test(
  "tolerates an unaccepted deviation without a parity block",
  async () => {
    await withTempLedger(async (directory) => {
      await Bun.write(path.join(directory, "OR-99-version.md"), ledgerText("pending owner", false))
      const result = await runParity(
        ["--reference", fakeServer, "--candidate", fakeServer, "--deviations", directory, infoScenario],
        { PARITY_CANDIDATE_ENV: JSON.stringify({ FAKE_VERSION: "other" }) },
      )
      expect(result.exitCode).toBe(1)
      expect(result.output).not.toContain("has no parity block")
    })
  },
  60_000,
)
