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
  multipartFixtures: { fixtures: Array<unknown> }
}

type ExportedError = {
  input: Record<string, unknown>
  reason: Record<string, unknown>
  retryable: boolean
}

type ExportedRealtime = {
  clockEpochMs: number
  cases: Array<{
    name: string
    input: Record<string, unknown>
    requests: Array<unknown>
    result?: unknown
    error?: unknown
  }>
}

type ExportedAdc = {
  clockEpochMs: number
  cases: Array<{
    name: string
    files: Record<string, string>
    env: Record<string, string>
    steps: Array<{ requests: Array<unknown>; token?: string; error?: string }>
  }>
}

type ExportedProviderEntries = {
  clockEpochMs: number
  cases: Array<{
    name: string
    specifier: string
    modelID: string
    settings: Record<string, unknown>
    env: Record<string, string>
    kind: string
    routeId: string
    descriptor: Record<string, unknown> | null
    prepared: unknown
    error: { message: string } | null
  }>
}

export const GROUPS: Record<string, { protocols: string[] }> = {
  "anthropic-messages": { protocols: ["anthropic-messages"] },
  "openai-chat": { protocols: ["openai-chat"] },
  "open-responses": { protocols: ["open-responses", "openai-responses"] },
  "openai-responses-websocket": { protocols: ["openai-responses"] },
  gemini: { protocols: ["gemini"] },
  "bedrock-converse": { protocols: ["bedrock-converse"] },
  "wrapped-core": { protocols: ["openai-chat", "anthropic-messages", "openai-responses", "open-responses"] },
  gateways: { protocols: ["openai-chat", "anthropic-messages", "openai-responses", "open-responses"] },
  "chat-variants": { protocols: ["openai-chat", "anthropic-messages", "open-responses"] },
  "vendor-variants": {
    protocols: [
      "alibaba-chat",
      "alibaba-messages",
      "alibaba-responses",
      "meta-messages",
      "meta-responses",
      "openai-chat",
      "xai-responses",
    ],
  },
  "native-vendors": { protocols: ["cohere-chat", "gemini", "google-interactions", "openai-chat"] },
  embeddings: {
    protocols: ["openai-embeddings", "google-embed-content", "google-vertex-predict-embeddings"],
  },
  decisions: {
    protocols: ["system-one", "baseten-system-one", "openai-decisions", "google-decisions", "cloudflare-run-decisions"],
  },
  images: {
    protocols: [
      "openai-images",
      "alibaba-images",
      "cloudflare-workers-ai-images",
      "google-images",
      "meta-images",
      "minimax-images",
      "xai-images",
      "zai-images",
    ],
  },
  video: {
    protocols: ["google-interactions-video", "minimax-video", "openrouter-video", "xai-video", "seedance-video"],
  },
  speech: {
    protocols: ["openai-speech", "cartesia-speech", "deepgram-speech", "elevenlabs-speech", "google-speech"],
  },
  transcription: {
    protocols: [
      "openai-transcription",
      "assemblyai-transcription",
      "deepgram-transcription",
      "elevenlabs-transcription",
      "google-transcription",
    ],
  },
  extraction: { protocols: ["cloudflare-run-extraction"] },
}

export const EXPECTED_CASSETTES = 313
const EXPECTED_ADC_CASES = 11
const EXPECTED_REALTIME_CASES = 8

const exporter = `${tree}/exporter`
await mkdir(exporter, { recursive: true })
for (const name of ["capture", "scenarios", "errors", "adc", "realtime", "providers"])
  await Bun.write(`${exporter}/${name}.ts`, Bun.file(`${root}script/llm/${name}.ts`))

const scenarios = await run<ScenarioOutput>("scenarios")
const errors = await run<Array<ExportedError>>("errors")
const adc = await run<ExportedAdc>("adc")
const realtime = await run<ExportedRealtime>("realtime")
const providerEntries = await run<ExportedProviderEntries>("providers")

if (adc.cases.length !== EXPECTED_ADC_CASES) fail(`expected ${EXPECTED_ADC_CASES} ADC cases, got ${adc.cases.length}`)
if (realtime.cases.length !== EXPECTED_REALTIME_CASES)
  fail(`expected ${EXPECTED_REALTIME_CASES} realtime cases, got ${realtime.cases.length}`)

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
await Bun.write(`${output}/google-adc.json`, `${JSON.stringify(adc, null, 2)}\n`)
await Bun.write(`${output}/multipart-fixtures.json`, `${JSON.stringify(scenarios.multipartFixtures, null, 2)}\n`)
await Bun.write(`${output}/realtime.json`, `${JSON.stringify(realtime, null, 2)}\n`)
await Bun.write(`${output}/provider-entries.json`, `${JSON.stringify(providerEntries, null, 2)}\n`)
console.log(
  `llm: ${scenarios.cassettes.length} cassettes, ${errors.length} provider errors, ${adc.cases.length} ADC cases, ${realtime.cases.length} realtime cases, ${providerEntries.cases.length} provider entry cases -> ${output}`,
)

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
