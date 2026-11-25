import fs from "node:fs/promises"
import path from "node:path"
import { pathToFileURL } from "node:url"
import { compareCaptures, PARTS, selectCapture, type Part } from "./lib/compare"
import {
  coverageReport,
  coverageSummary,
  loadSurface,
  writeCoverage,
  type RequestRecord,
  type RunCoverage,
} from "./lib/coverage"
import { deviationWarnings, explain, loadDeviations, type Deviation, type Explained } from "./lib/deviations"
import { runSide } from "./lib/drive"
import { startModelEndpoint, type ModelEndpoint } from "./lib/model"
import { normalizeCapture } from "./lib/normalize"
import { createSandbox, removeSandbox, resetSandbox } from "./lib/sandbox"
import type { Scenario } from "./lib/scenario"
import { stopServersOnSignal } from "./lib/server"

const parityRoot = import.meta.dir
const repositoryRoot = path.resolve(parityRoot, "../../..")
const scenariosRoot = path.join(parityRoot, "scenarios")
const deviationsRoot = path.join(parityRoot, "deviations")
const outputRoot = process.env.PARITY_OUT_DIR ? path.resolve(process.env.PARITY_OUT_DIR) : path.join(parityRoot, "out")
const usage =
  "usage: bun run parity/run.ts --candidate <ts|path> [--reference <ts|path>] [--repeat <n>] " +
  "[--deviations <dir>] [--only <parts>] [--tables <names>] [--allow-pending] [scenario...]"
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

type Options = {
  reference: string
  candidate: string
  repeat: number
  deviations: string
  files: string[]
  parts: Set<Part>
  tables: string[] | undefined
  allowPending: boolean
}
type Capture = {
  reference: Record<string, unknown>
  candidate: Record<string, unknown>
  differences: ReturnType<typeof compareCaptures>
  coverage: { requests: RequestRecord[]; events: unknown[] }
}

async function main() {
  stopServersOnSignal()
  const options = parseArguments(Bun.argv.slice(2))
  const deviations = await readDeviations(options.deviations)
  requireParityBlocks(deviations, options.allowPending)
  const candidateEnv = readCandidateEnv()
  await warnAboutPin()
  const files = await scenarioFiles(options.files)
  if (files.length === 0) usageError("no scenario files found")
  const scenarios = await loadScenarios(files)
  requireEventScenarios(scenarios, options.parts)
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
      const { explained, unexplained } = explain(
        outcome.capture.differences,
        entry.name,
        deviations,
        options.allowPending,
      )
      for (const item of explained) matched.add(item.id)
      if (unexplained.length === 0) {
        console.log(passLine(entry.name, explained))
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
    const comparedWebsockets = coverageRuns.some((run) => run.requests.some((request) => request.protocol === "ws"))
    const coverage = coverageReport(surface, coverageRuns, {
      operations: options.parts.has("steps"),
      events: options.parts.has("events"),
      websockets: options.parts.size === PARTS.length || (options.parts.has("steps") && comparedWebsockets),
    })
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

function passLine(name: string, explained: Explained[]) {
  const pending = explained.filter((item) => item.pending)
  if (pending.length > 0) return `PASS ${name} (${pending.length} explained by pending ${explanationIDs(pending)})`
  if (explained.length === 0) return `PASS ${name}`
  return `PASS ${name} (${explained.length} explained by ${explanationIDs(explained)})`
}

function explanationIDs(explained: Explained[]) {
  return [...new Set(explained.map((item) => item.id))].sort().join(", ")
}

function requireParityBlocks(deviations: Deviation[], allowPending: boolean) {
  const missing = deviations.find(
    (deviation) => (deviation.accepted || (allowPending && deviation.pending)) && deviation.rules === undefined,
  )
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
  let parts = new Set<Part>(PARTS)
  let tables: string[] | undefined
  let allowPending = false
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index]
    if (argument === "--allow-pending") {
      allowPending = true
      continue
    }
    if (
      argument === "--reference" ||
      argument === "--candidate" ||
      argument === "--repeat" ||
      argument === "--deviations" ||
      argument === "--only" ||
      argument === "--tables"
    ) {
      const value = argv[index + 1]
      if (value === undefined || value.startsWith("--")) usageError(`${argument} requires a value`)
      index += 1
      if (argument === "--reference") reference = value
      if (argument === "--candidate") candidate = value
      if (argument === "--repeat") repeat = parseRepeat(value)
      if (argument === "--deviations") deviations = value
      if (argument === "--only") parts = parseParts(value)
      if (argument === "--tables") tables = parseTables(value)
      continue
    }
    if (argument.startsWith("-")) usageError(`unknown flag ${argument}`)
    files.push(argument)
  }
  if (candidate === undefined) usageError("--candidate is required")
  if (tables !== undefined && !parts.has("database")) usageError("--tables requires database in --only")
  if (parts.has("logs") && !parts.has("events")) usageError("--only logs requires events")
  return { reference, candidate, repeat, deviations, files, parts, tables, allowPending }
}

function parseParts(value: string): Set<Part> {
  const parts = new Set<Part>()
  for (const part of value.split(",")) {
    if (!isPart(part)) usageError(`--only must list parts from ${PARTS.join(", ")}, got ${value}`)
    parts.add(part)
  }
  return parts
}

function isPart(value: string): value is Part {
  return PARTS.some((part) => part === value)
}

function parseTables(value: string): string[] {
  const names: string[] = []
  for (const name of value.split(",")) {
    if (name.length === 0) usageError(`--tables must be a comma list of table names, got ${value}`)
    if (!names.includes(name)) names.push(name)
  }
  return names
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

function requireEventScenarios(scenarios: { name: string; scenario: Scenario }[], parts: Set<Part>) {
  if (parts.has("events")) return
  const needsEvents = scenarios.find((entry) => entry.scenario.steps.some((step) => "waitEvent" in step))
  if (needsEvents === undefined) return
  console.error(`FAIL ${needsEvents.name}: scenario needs events`)
  process.exit(2)
}

async function captureScenario(scenario: Scenario, options: Options, candidateEnv: Record<string, string>) {
  const box = await createSandbox()
  const captureEvents = options.parts.has("events")
  try {
    const endpoint = await startModelEndpoint()
    try {
      const template = { work: box.work, root: box.root }
      const replies = scenario.model ?? []
      endpoint.load(replies, template)
      const referenceStarted = Date.now()
      const referenceRun = await withModelFailure(
        endpoint,
        runSide(scenario, options.reference, box, endpoint, { events: captureEvents }),
      )
      requireAllReplies(endpoint)
      const referenceEnd = Date.now()
      const referenceCapture = normalizeCapture(referenceRun.capture, {
        root: box.root,
        start: referenceStarted,
        end: referenceEnd,
      }) as Record<string, unknown>
      requireMatchedTables(referenceCapture, options)
      await resetSandbox(box.root)
      endpoint.load(replies, template)
      const candidateStarted = Date.now()
      const candidateRun = await withModelFailure(
        endpoint,
        runSide(scenario, options.candidate, box, endpoint, { events: captureEvents, env: candidateEnv }),
      )
      requireAllReplies(endpoint)
      const candidateEnd = Date.now()
      const candidateCapture = normalizeCapture(candidateRun.capture, {
        root: box.root,
        start: candidateStarted,
        end: candidateEnd,
      }) as Record<string, unknown>
      const differences = compareCaptures(
        selectCapture(referenceCapture, options.parts, options.tables),
        selectCapture(candidateCapture, options.parts, options.tables),
      )
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

function requireMatchedTables(reference: Record<string, unknown>, options: Options) {
  const tables = options.tables
  if (!options.parts.has("database") || tables === undefined) return
  const databases = isObject(reference.database) ? Object.values(reference.database) : []
  const matched = databases.some((entry) => {
    if (!isObject(entry) || !isObject(entry.tables)) return false
    const captured = entry.tables
    return tables.some((table) => Object.hasOwn(captured, table))
  })
  if (matched) return
  throw new Error(`--tables ${tables.join(",")} matched no table`)
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

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

function message(error: unknown) {
  return error instanceof Error ? error.message : String(error)
}

await main().catch((error: unknown) => {
  console.error(`parity: ${message(error)}`)
  process.exit(2)
})
