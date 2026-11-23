import fs from "node:fs/promises"
import path from "node:path"
import { pathToFileURL } from "node:url"
import { compareCaptures } from "./lib/compare"
import {
  coverageReport,
  coverageSummary,
  loadSurface,
  writeCoverage,
  type RequestRecord,
  type RunCoverage,
} from "./lib/coverage"
import { deviationWarnings, explain, loadDeviations, type Deviation } from "./lib/deviations"
import { runSide } from "./lib/drive"
import { startModelEndpoint, type ModelEndpoint } from "./lib/model"
import { normalizeCapture } from "./lib/normalize"
import { createSandbox, removeSandbox, resetSandbox } from "./lib/sandbox"
import type { Scenario } from "./lib/scenario"

const parityRoot = import.meta.dir
const repositoryRoot = path.resolve(parityRoot, "../../..")
const scenariosRoot = path.join(parityRoot, "scenarios")
const deviationsRoot = path.join(parityRoot, "deviations")
const outputRoot = process.env.PARITY_OUT_DIR ? path.resolve(process.env.PARITY_OUT_DIR) : path.join(parityRoot, "out")
const usage =
  "usage: bun run parity/run.ts --candidate <ts|path> [--reference <ts|path>] [--repeat <n>] " +
  "[--deviations <dir>] [scenario...]"
const referencePaths = [
  "packages/cli/src",
  "packages/core/src",
  "packages/server/src",
  "packages/protocol",
  "packages/schema/src",
  "packages/ai/src",
  "packages/util/src",
  "packages/plugin/src",
  "packages/codemode/src",
]

type Options = { reference: string; candidate: string; repeat: number; deviations: string; files: string[] }
type Capture = {
  reference: unknown
  candidate: unknown
  differences: ReturnType<typeof compareCaptures>
  coverage: { requests: RequestRecord[]; events: unknown[] }
}

async function main() {
  const options = parseArguments(Bun.argv.slice(2))
  const deviations = await readDeviations(options.deviations)
  requireParityBlocks(deviations)
  const candidateEnv = readCandidateEnv()
  await warnAboutPin()
  const files = await scenarioFiles(options.files)
  if (files.length === 0) usageError("no scenario files found")
  const scenarios = await loadScenarios(files)
  let differencesFound = false
  let errorFound = false
  const matched = new Set<string>()
  const coverageRuns: RunCoverage[] = []
  for (let repetition = 1; repetition <= options.repeat && !errorFound; repetition += 1) {
    let failed = false
    for (const entry of scenarios) {
      const outcome = await captureScenario(entry.scenario, options, candidateEnv).then(
        (capture) => ({ ok: true as const, capture }),
        (error: unknown) => ({ ok: false as const, message: message(error) }),
      )
      if (!outcome.ok) {
        console.error(`FAIL ${entry.name}: ${outcome.message}`)
        errorFound = true
        break
      }
      await writeOutputs(entry.name, outcome.capture)
      coverageRuns.push({ name: entry.name, ...outcome.capture.coverage })
      const { explained, unexplained } = explain(outcome.capture.differences, entry.name, deviations)
      for (const item of explained) matched.add(item.id)
      if (unexplained.length === 0) {
        const summary = [...new Set(explained.map((item) => item.id))].sort().join(", ")
        console.log(
          explained.length === 0
            ? `PASS ${entry.name}`
            : `PASS ${entry.name} (${explained.length} explained by ${summary})`,
        )
        continue
      }
      failed = true
      differencesFound = true
      console.log(`FAIL ${entry.name}: ${unexplained.length} difference(s)`)
      for (const difference of unexplained.slice(0, 20))
        console.log(`  ${difference.path}: ${format(difference.reference)} != ${format(difference.candidate)}`)
    }
    console.log(`run ${repetition}/${options.repeat}`)
    if (failed) break
  }
  if (coverageRuns.length > 0) {
    const surface = await loadSurface(repositoryRoot)
    const coverage = coverageReport(surface, coverageRuns)
    await writeCoverage(outputRoot, coverage)
    console.log(coverageSummary(coverage))
  }
  if (options.files.length === 0 && !errorFound)
    for (const warning of deviationWarnings(deviations, matched)) console.log(warning)
  process.exit(errorFound ? 2 : differencesFound ? 1 : 0)
}

async function readDeviations(directory: string) {
  try {
    return await loadDeviations(directory)
  } catch (error) {
    console.error(`parity: cannot read deviations from ${directory}: ${message(error)}`)
    process.exit(2)
  }
}

function requireParityBlocks(deviations: Deviation[]) {
  const missing = deviations.find((deviation) => deviation.accepted && deviation.rules === undefined)
  if (missing === undefined) return
  console.error(`parity: deviation ${missing.id} has no parity block`)
  process.exit(2)
}

function parseArguments(argv: string[]): Options {
  const files: string[] = []
  let reference = "ts"
  let candidate: string | undefined
  let repeat = 1
  let deviations = deviationsRoot
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index]
    if (
      argument === "--reference" ||
      argument === "--candidate" ||
      argument === "--repeat" ||
      argument === "--deviations"
    ) {
      const value = argv[index + 1]
      if (value === undefined || value.startsWith("--")) usageError(`${argument} requires a value`)
      index += 1
      if (argument === "--reference") reference = value
      if (argument === "--candidate") candidate = value
      if (argument === "--repeat") repeat = parseRepeat(value)
      if (argument === "--deviations") deviations = value
      continue
    }
    if (argument.startsWith("-")) usageError(`unknown flag ${argument}`)
    files.push(argument)
  }
  if (candidate === undefined) usageError("--candidate is required")
  return { reference, candidate, repeat, deviations, files }
}

function parseRepeat(value: string) {
  const repeat = Number(value)
  if (!Number.isInteger(repeat) || repeat < 1) usageError(`--repeat must be a positive integer, got ${value}`)
  return repeat
}

function usageError(detail: string): never {
  console.error(`parity: ${detail}`)
  console.error(usage)
  process.exit(2)
}

function readCandidateEnv(): Record<string, string> {
  const raw = process.env.PARITY_CANDIDATE_ENV
  if (raw === undefined || raw.length === 0) return {}
  const parsed = parseJSON(raw)
  if (parsed === undefined || typeof parsed !== "object" || parsed === null || Array.isArray(parsed))
    usageError("PARITY_CANDIDATE_ENV must be a JSON object of strings")
  const entries = Object.entries(parsed)
  if (entries.some(([, value]) => typeof value !== "string"))
    usageError("PARITY_CANDIDATE_ENV must be a JSON object of strings")
  return Object.fromEntries(entries) as Record<string, string>
}

function parseJSON(raw: string): unknown {
  try {
    return JSON.parse(raw)
  } catch {
    return undefined
  }
}

async function warnAboutPin() {
  const file = Bun.file(path.join(parityRoot, "../upstream-pin"))
  if (!(await file.exists())) return
  const pin = (await file.text()).trim()
  if (pin.length === 0) return
  if (!hasCommit(pin)) return
  const changed = changedFiles(pin)
  if (changed.length === 0) return
  console.log(`warning: the TypeScript reference differs from the pin in ${changed.length} file(s)`)
}

function hasCommit(pin: string) {
  const result = Bun.spawnSync(["git", "cat-file", "-e", `${pin}^{commit}`], {
    cwd: repositoryRoot,
    stdout: "ignore",
    stderr: "ignore",
  })
  return result.exitCode === 0
}

function changedFiles(pin: string) {
  return [
    ["git", "diff", "--name-only", pin, "--", ...referencePaths],
    ["git", "ls-files", "--others", "--exclude-standard", "--", ...referencePaths],
  ].flatMap((command) => {
    const result = Bun.spawnSync(command, { cwd: repositoryRoot, stdout: "pipe", stderr: "ignore" })
    if (result.exitCode !== 0) return []
    return result.stdout
      .toString()
      .split("\n")
      .filter((line) => line.length > 0)
  })
}

async function scenarioFiles(files: string[]) {
  if (files.length > 0) return files
  const entries = await fs.readdir(scenariosRoot)
  return entries
    .filter((name) => name.endsWith(".scenario.ts"))
    .sort()
    .map((name) => path.join(scenariosRoot, name))
}

function scenarioName(file: string) {
  const relative = path.relative(scenariosRoot, path.resolve(file))
  const name = relative.startsWith("..") || path.isAbsolute(relative) ? path.basename(file) : relative
  return name.replace(/\.scenario\.ts$/, "")
}

async function loadScenarios(files: string[]) {
  const scenarios: { name: string; scenario: Scenario }[] = []
  for (const file of files) {
    const name = scenarioName(file)
    try {
      scenarios.push({ name, scenario: await loadScenario(file) })
    } catch (error) {
      console.error(`FAIL ${name}: ${message(error)}`)
      process.exit(2)
    }
  }
  return scenarios
}

async function loadScenario(file: string) {
  const module: { default?: unknown } = await import(pathToFileURL(path.resolve(file)).href)
  const scenario = module.default
  if (typeof scenario !== "object" || scenario === null || !Array.isArray((scenario as Scenario).steps))
    throw new Error(`${file} does not default-export a scenario`)
  return scenario as Scenario
}

async function captureScenario(scenario: Scenario, options: Options, candidateEnv: Record<string, string>) {
  const box = await createSandbox()
  try {
    const endpoint = await startModelEndpoint()
    try {
      const template = { work: box.work, root: box.root }
      const replies = scenario.model ?? []
      endpoint.load(replies, template)
      const referenceStarted = Date.now()
      const referenceRun = await withModelFailure(endpoint, runSide(scenario, options.reference, box, endpoint))
      requireAllReplies(endpoint)
      const referenceEnd = Date.now()
      await resetSandbox(box.root)
      endpoint.load(replies, template)
      const candidateStarted = Date.now()
      const candidateRun = await withModelFailure(
        endpoint,
        runSide(scenario, options.candidate, box, endpoint, candidateEnv),
      )
      requireAllReplies(endpoint)
      const candidateEnd = Date.now()
      const referenceCapture = normalizeCapture(referenceRun.capture, {
        root: box.root,
        start: referenceStarted,
        end: referenceEnd,
      })
      const candidateCapture = normalizeCapture(candidateRun.capture, {
        root: box.root,
        start: candidateStarted,
        end: candidateEnd,
      })
      const differences = compareCaptures(referenceCapture, candidateCapture)
      return {
        reference: referenceCapture,
        candidate: candidateCapture,
        differences,
        coverage: { requests: referenceRun.requests, events: referenceRun.capture.events },
      } satisfies Capture
    } finally {
      await endpoint.stop()
    }
  } finally {
    await removeSandbox(box.root)
  }
}

function withModelFailure<T>(endpoint: ModelEndpoint, run: Promise<T>) {
  return run.catch((error: unknown) => {
    const failure = endpoint.failures()[0]
    throw failure === undefined ? error : new Error(`scenario: ${failure}`, { cause: error })
  })
}

function requireAllReplies(endpoint: ModelEndpoint) {
  const failure = endpoint.failures()[0]
  if (failure !== undefined) throw new Error(`scenario: ${failure}`)
  const unused = endpoint.unused()
  if (unused === 0) return
  throw new Error(`scenario: ${unused} unused model ${unused === 1 ? "reply" : "replies"}`)
}

async function writeOutputs(name: string, capture: Capture) {
  const directory = path.join(outputRoot, name)
  await fs.mkdir(directory, { recursive: true })
  await Promise.all([
    writeJSON(path.join(directory, "reference.json"), capture.reference),
    writeJSON(path.join(directory, "candidate.json"), capture.candidate),
    writeJSON(path.join(directory, "differences.json"), capture.differences),
  ])
}

async function writeJSON(file: string, value: unknown) {
  await Bun.write(file, JSON.stringify(value, null, 2) + "\n")
}

function format(value: unknown) {
  if (value === undefined) return "undefined"
  return JSON.stringify(value) ?? String(value)
}

function message(error: unknown) {
  return error instanceof Error ? error.message : String(error)
}

await main().catch((error: unknown) => {
  console.error(`parity: ${message(error)}`)
  process.exit(2)
})
