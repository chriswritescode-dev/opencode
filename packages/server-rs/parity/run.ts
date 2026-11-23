import fs from "node:fs/promises"
import path from "node:path"
import { pathToFileURL } from "node:url"
import { compareCaptures } from "./lib/compare"
import { runSide } from "./lib/drive"
import { normalizeCapture } from "./lib/normalize"
import { createSandbox, resetSandbox } from "./lib/sandbox"
import type { Scenario } from "./lib/scenario"

const parityRoot = import.meta.dir
const repositoryRoot = path.resolve(parityRoot, "../../..")
const scenariosRoot = path.join(parityRoot, "scenarios")
const outputRoot = path.join(parityRoot, "out")
const usage =
  "usage: bun run parity/run.ts --candidate <ts|path> [--reference <ts|path>] [--repeat <n>] [scenario...]"
const referencePaths = [
  "packages/cli/src",
  "packages/core/src",
  "packages/server/src",
  "packages/protocol",
  "packages/schema/src",
  "packages/ai/src",
  "packages/util/src",
]

type Options = { reference: string; candidate: string; repeat: number; files: string[] }
type Capture = { reference: unknown; candidate: unknown; differences: ReturnType<typeof compareCaptures> }

async function main() {
  const options = parseArguments(Bun.argv.slice(2))
  const candidateEnv = readCandidateEnv()
  await warnAboutPin()
  const files = await scenarioFiles(options.files)
  if (files.length === 0) usageError("no scenario files found")
  const scenarios = await loadScenarios(files)
  let differencesFound = false
  let errorFound = false
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
      if (outcome.capture.differences.length === 0) {
        console.log(`PASS ${entry.name}`)
        continue
      }
      failed = true
      differencesFound = true
      console.log(`FAIL ${entry.name}: ${outcome.capture.differences.length} difference(s)`)
      for (const difference of outcome.capture.differences.slice(0, 20))
        console.log(`  ${difference.path}: ${format(difference.reference)} != ${format(difference.candidate)}`)
    }
    console.log(`run ${repetition}/${options.repeat}`)
    if (failed) break
  }
  process.exit(errorFound ? 2 : differencesFound ? 1 : 0)
}

function parseArguments(argv: string[]): Options {
  const files: string[] = []
  let reference = "ts"
  let candidate: string | undefined
  let repeat = 1
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index]
    if (argument === "--reference" || argument === "--candidate" || argument === "--repeat") {
      const value = argv[index + 1]
      if (value === undefined || value.startsWith("--")) usageError(`${argument} requires a value`)
      index += 1
      if (argument === "--reference") reference = value
      if (argument === "--candidate") candidate = value
      if (argument === "--repeat") repeat = parseRepeat(value)
      continue
    }
    if (argument.startsWith("-")) usageError(`unknown flag ${argument}`)
    files.push(argument)
  }
  if (candidate === undefined) usageError("--candidate is required")
  return { reference, candidate, repeat, files }
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
  const result = Bun.spawnSync(["git", "diff", "--name-only", pin, "HEAD", "--", ...referencePaths], {
    cwd: repositoryRoot,
    stdout: "pipe",
    stderr: "ignore",
  })
  if (result.exitCode !== 0) return []
  return result.stdout.toString().split("\n").filter((line) => line.length > 0)
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
    const referenceStarted = Date.now()
    const reference = await runSide(scenario, options.reference, box)
    const referenceEnd = Date.now()
    await resetSandbox(box.root)
    const candidateStarted = Date.now()
    const candidate = await runSide(scenario, options.candidate, box, candidateEnv)
    const candidateEnd = Date.now()
    const referenceCapture = normalizeCapture(reference, {
      root: box.root,
      start: referenceStarted,
      end: referenceEnd,
    })
    const candidateCapture = normalizeCapture(candidate, {
      root: box.root,
      start: candidateStarted,
      end: candidateEnd,
    })
    const differences = compareCaptures(referenceCapture, candidateCapture)
    return { reference: referenceCapture, candidate: candidateCapture, differences } satisfies Capture
  } finally {
    await fs.rm(box.root, { recursive: true, force: true })
  }
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

await main()
