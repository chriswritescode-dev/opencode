import { mkdir, rm } from "node:fs/promises"
import { dirname } from "node:path"
import { upstreamTree } from "../upstream"

const root = new URL("../..", import.meta.url).pathname
const tree = await upstreamTree()
const output = `${root}crates/ai/tests/corpus`

type CapturedCassette = {
  cassette: string
  protocol: string
  routeId: string
  clockEpochMs: number
  steps: Array<unknown>
}

type ScenarioOutput = {
  cassettes: Array<CapturedCassette>
  groups: Record<string, string>
}

type ExportedError = {
  input: Record<string, unknown>
  reason: Record<string, unknown>
  retryable: boolean
}

export const GROUPS: Record<string, { protocols: string[] }> = {
  "anthropic-messages": { protocols: ["anthropic-messages"] },
  "openai-chat": { protocols: ["openai-chat"] },
  "open-responses": { protocols: ["open-responses", "openai-responses"] },
  "openai-responses-websocket": { protocols: ["openai-responses"] },
  gemini: { protocols: ["gemini"] },
  "bedrock-converse": { protocols: ["bedrock-converse"] },
}

export const EXPECTED_CASSETTES = 51

const exporter = `${tree}/exporter`
await mkdir(exporter, { recursive: true })
for (const name of ["capture", "scenarios", "errors"])
  await Bun.write(`${exporter}/${name}.ts`, Bun.file(`${root}script/llm/${name}.ts`))

const scenarios = await run<ScenarioOutput>("scenarios")
const errors = await run<Array<ExportedError>>("errors")

if (scenarios.cassettes.length !== EXPECTED_CASSETTES)
  fail(`expected ${EXPECTED_CASSETTES} cassettes, got ${scenarios.cassettes.length}`)
const names = new Set<string>()
const index: Array<{ cassette: string; protocol: string; group: string }> = []
for (const cassette of scenarios.cassettes) {
  if (names.has(cassette.cassette)) fail(`duplicate cassette ${cassette.cassette}`)
  names.add(cassette.cassette)
  const group = scenarios.groups[cassette.cassette]
  if (group === undefined) fail(`cassette ${cassette.cassette} has no group`)
  const definition = GROUPS[group]
  if (definition === undefined) fail(`cassette ${cassette.cassette} has unknown group ${group}`)
  if (!definition.protocols.includes(cassette.protocol))
    fail(`cassette ${cassette.cassette} has protocol ${cassette.protocol} outside group ${group}`)
  index.push({ cassette: cassette.cassette, protocol: cassette.protocol, group })
}
index.sort((a, b) => (a.cassette < b.cassette ? -1 : 1))

await rm(`${output}/llm`, { recursive: true, force: true })
for (const cassette of scenarios.cassettes) {
  const file = `${output}/llm/${cassette.cassette}.json`
  await mkdir(dirname(file), { recursive: true })
  await Bun.write(file, `${JSON.stringify(cassette, null, 2)}\n`)
}
await mkdir(`${output}/llm`, { recursive: true })
await Bun.write(`${output}/llm/index.json`, `${JSON.stringify({ cassettes: index }, null, 2)}\n`)
await Bun.write(`${output}/provider-errors.json`, `${JSON.stringify({ errors }, null, 2)}\n`)
console.log(`llm: ${scenarios.cassettes.length} cassettes, ${errors.length} provider errors -> ${output}`)

async function run<T>(name: string) {
  const proc = Bun.spawn(["bun", "run", `${exporter}/${name}.ts`], {
    cwd: `${tree}/packages/ai`,
    env: { ...process.env, CI: "true", OPENCODE_UPSTREAM_TREE: tree },
    stdout: "pipe",
    stderr: "inherit",
  })
  const stdout = await new Response(proc.stdout).text()
  const code = await proc.exited
  if (code !== 0) fail(`exporter/${name}.ts exited with ${code}`)
  return JSON.parse(stdout) as T
}

function fail(message: string): never {
  console.error(`llm: ${message}`)
  process.exit(1)
}
