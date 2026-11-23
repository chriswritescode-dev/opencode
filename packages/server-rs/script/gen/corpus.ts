import { mkdir } from "node:fs/promises"
import { Arbitrary, Effect, Schema } from "effect"
import {
  contractErrors,
  contractEvents,
  contractRequests,
  contractResponses,
  type RequestRoot,
} from "../contract/model"
import { upstreamTree } from "../upstream"

const root = new URL("../..", import.meta.url).pathname
const file = `${root}crates/contract/tests/corpus/schemas.json`
const eventsFile = `${root}crates/contract/tests/corpus/events.json`
const requestsFile = `${root}crates/contract/tests/corpus/requests.json`
const tree = await upstreamTree()
const queryString = await import(`${tree}/node_modules/effect/src/http/FindMyWay/internal/queryString.ts`)

type JsonCodec = Schema.Codec<unknown, unknown>
type Sample = { input: string; expected: string }
type Root = { name: string; valid: Array<Sample>; invalid: Array<string>; skipped: number }

const numbers = [
  "0",
  "-0",
  "0.1",
  "1e21",
  "1e-7",
  "123456789012345680000",
  "5e-324",
  "1.7976931348623157e308",
  "9007199254740993",
]

const errors = await contractErrors()
const events = await contractEvents()
const responses = await contractResponses(events)

const roots: Array<Root> = []
for (const error of errors) roots.push(await schemaRoot(error.tag, error.schema))
roots.push(numbersRoot())
for (const event of events.public) roots.push(await schemaRoot(event.type, event.schema))
for (const event of events.durable) roots.push(await schemaRoot(event.key, event.schema))
roots.push(await schemaRoot("SessionLogItem", events.sessionLog))
for (const id of events.ids) roots.push(await schemaRoot(id.identifier, id.schema))
for (const response of responses) roots.push(await schemaRoot(`operation:${response.operationId}`, response.schema))
roots.sort((a, b) => (a.name < b.name ? -1 : 1))

const lines = roots.map(
  (item) => `${JSON.stringify(item.name)}:${JSON.stringify({ valid: item.valid, invalid: item.invalid })}`,
)
await mkdir(`${root}crates/contract/tests/corpus`, { recursive: true })
await Bun.write(file, `{\n${lines.join(",\n")}\n}\n`)
await Bun.write(
  eventsFile,
  `${JSON.stringify(
    {
      public: events.public.map((event) => event.type),
      durable: events.durable.map((event) => [event.type, event.version, event.aggregate]),
      sessionLog: events.sessionLogTypes,
    },
    null,
    2,
  )}\n`,
)

const requestRoots = await contractRequests()
const requestCorpus: Record<
  string,
  Partial<Record<RequestRoot["slot"], { valid: Array<RequestSample>; invalid: Array<RequestRaw> }>>
> = {}
let requestValid = 0
let requestInvalid = 0
let requestSkipped = 0
for (const request of requestRoots) {
  const corpus = await requestSlotCorpus(request)
  requestValid += corpus.valid.length
  requestInvalid += corpus.invalid.length
  requestSkipped += corpus.skipped
  const operation = (requestCorpus[request.operationId] ??= {})
  operation[request.slot] = { valid: corpus.valid, invalid: corpus.invalid }
}
const requestLines = Object.keys(requestCorpus)
  .toSorted()
  .map((operation) => `${JSON.stringify(operation)}:${JSON.stringify(requestCorpus[operation])}`)
await Bun.write(requestsFile, `{\n${requestLines.join(",\n")}\n}\n`)
console.log(
  `requests: ${requestRoots.length} roots, ${requestValid} valid, ${requestInvalid} invalid, ${requestSkipped} skipped`,
)

const total = (pick: (item: Root) => number) => roots.reduce((sum, item) => sum + pick(item), 0)
console.log(
  `corpus: ${roots.length} roots, ${total((item) => item.valid.length)} valid, ${total((item) => item.invalid.length)} invalid, ${total((item) => item.skipped)} skipped`,
)
for (const item of roots)
  console.log(`corpus: ${item.name} valid=${item.valid.length} invalid=${item.invalid.length} skipped=${item.skipped}`)

async function schemaRoot(name: string, schema: Schema.Top): Promise<Root> {
  const codec = Schema.toCodecJson(schema as unknown as JsonCodec)
  const samples = await Effect.runPromise(
    Arbitrary.sampleEffect(Arbitrary.schema(schema), { seed: name, count: 12, size: 4 }),
  )
  const valid: Array<Sample> = []
  let skipped = 0
  for (const sample of samples) {
    const encoded = Schema.encodeUnknownSync(codec)(sample)
    if (hasLoneSurrogate(encoded)) {
      skipped += 1
      continue
    }
    if (valid.length === 3) continue
    const decoded = Schema.decodeUnknownSync(codec)(JSON.parse(JSON.stringify(encoded)))
    const input = canonical(codec, encoded, decoded)
    Schema.decodeUnknownSync(codec)(JSON.parse(input))
    valid.push({ input, expected: input })
  }
  if (valid.length === 0) throw new Error(`corpus: ${name} produced no surrogate-free sample`)
  const first = JSON.parse(valid[0].input)
  const known = new Set(valid.map((sample) => sample.input))
  const invalid: Array<string> = []
  const add = (candidate: unknown) => {
    if (hasLoneSurrogate(candidate)) {
      skipped += 1
      return
    }
    const input = JSON.stringify(candidate)
    if (known.has(input)) return
    if (Schema.decodeUnknownExit(codec)(candidate)._tag !== "Failure") return
    known.add(input)
    invalid.push(input)
  }
  if (!isObject(first)) {
    for (const candidate of [null, true, 0, "", " ", "!", `!${String(first)}`, `${String(first)}!`, "__invalid__"])
      add(candidate)
    return { name, valid, invalid, skipped }
  }

  const excess = { ...first, __excess__: 123 }
  const excessInput = JSON.stringify(excess)
  const decoded = Schema.decodeUnknownExit(codec)(excess)
  if (decoded._tag === "Success") {
    const expected = canonical(codec, Schema.encodeUnknownSync(codec)(decoded.value), decoded.value)
    if (expected !== excessInput) valid.push({ input: excessInput, expected })
  }

  const keys = Object.keys(first)
  const withValue = (key: string, value: unknown) => ({ ...first, [key]: value })
  for (const key of keys) {
    const candidate = { ...first }
    delete candidate[key]
    add(candidate)
  }
  for (const key of keys) add(withValue(key, replacement(first[key])))
  for (const key of keys) {
    const value = first[key]
    if (typeof value === "string") add(withValue(key, `${value}!`))
  }
  for (const key of keys) {
    const value = first[key]
    if (typeof value !== "number") continue
    add(withValue(key, value + 0.5))
    add(withValue(key, value - 1e308))
  }
  for (const key of keys) {
    const value = first[key]
    if (typeof value === "boolean") add(withValue(key, !value))
  }
  if ("_tag" in first) add(withValue("_tag", "__unknown__"))
  if (typeof first.type === "string") add(withValue("type", `${first.type}.unknown`))
  return { name, valid, invalid, skipped }
}

function numbersRoot(): Root {
  const codec = Schema.toCodecJson(Schema.Number)
  return {
    name: "numbers",
    valid: numbers.map((input) => ({ input, expected: roundTrip(codec, input) })),
    invalid: [],
    skipped: 0,
  }
}

function roundTrip(codec: JsonCodec, input: string) {
  const decoded = Schema.decodeUnknownSync(codec)(JSON.parse(input))
  return JSON.stringify(Schema.encodeUnknownSync(codec)(decoded))
}

function replacement(value: unknown): unknown {
  if (value === null) return 0
  if (Array.isArray(value)) return {}
  if (typeof value === "string") return 123
  if (typeof value === "number") return "x"
  if (typeof value === "boolean") return 0
  return []
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

function canonical(codec: JsonCodec, encoded: unknown, decoded: unknown) {
  const stripped = JSON.stringify(withoutNullOptionals(encoded, decoded))
  return decodes(codec, stripped) ? stripped : JSON.stringify(encoded)
}

function decodes(codec: JsonCodec, input: string) {
  return Schema.decodeUnknownExit(codec)(JSON.parse(input))._tag === "Success"
}

function withoutNullOptionals(encoded: unknown, decoded: unknown): unknown {
  if (Array.isArray(encoded) && Array.isArray(decoded))
    return encoded.map((value, index) => withoutNullOptionals(value, decoded[index]))
  if (!isObject(encoded) || !isObject(decoded)) return encoded
  const result: Record<string, unknown> = {}
  for (const [key, value] of Object.entries(encoded)) {
    if (value === null && (!(key in decoded) || decoded[key] === undefined)) continue
    result[key] = withoutNullOptionals(value, decoded[key])
  }
  return result
}

function hasLoneSurrogate(value: unknown): boolean {
  if (typeof value === "string") {
    for (let index = 0; index < value.length; index++) {
      const unit = value.charCodeAt(index)
      if (unit >= 0xd800 && unit <= 0xdbff) {
        const next = value.charCodeAt(index + 1)
        if (!(next >= 0xdc00 && next <= 0xdfff)) return true
        index++
      } else if (unit >= 0xdc00 && unit <= 0xdfff) {
        return true
      }
    }
    return false
  }
  if (Array.isArray(value)) return value.some(hasLoneSurrogate)
  if (typeof value === "object" && value !== null)
    return Object.entries(value as Record<string, unknown>).some(
      ([key, item]) => hasLoneSurrogate(key) || hasLoneSurrogate(item),
    )
  return false
}

type RequestRaw = { params?: Record<string, string>; query?: string; headers?: Record<string, string>; body?: string }
type RequestSample = { raw: RequestRaw; expected: string }
type RequestSlotCorpus = { valid: Array<RequestSample>; invalid: Array<RequestRaw>; skipped: number }

async function requestSlotCorpus(request: RequestRoot): Promise<RequestSlotCorpus> {
  const samples = await Effect.runPromise(
    Arbitrary.sampleEffect(Arbitrary.schema(request.schema), {
      seed: `${request.operationId}:${request.slot}`,
      count: 12,
      size: 4,
    }),
  )
  const typed = Schema.toCodecJson(Schema.toType(request.schema) as unknown as JsonCodec)
  const wire =
    request.slot === "body"
      ? Schema.toCodecJson(request.schema as unknown as JsonCodec)
      : (request.schema as unknown as JsonCodec)
  const valid: Array<RequestSample> = []
  const seen = new Set<string>()
  let skipped = 0
  for (const sample of samples) {
    const raw = encodeRaw(request.slot, wire, sample)
    if (raw === undefined) {
      skipped += 1
      continue
    }
    const key = JSON.stringify(raw)
    if (seen.has(key)) continue
    let decoded: unknown
    try {
      decoded = decodeRaw(request.slot, request.schema, raw)
    } catch {
      skipped += 1
      continue
    }
    let expected: string
    try {
      expected =
        request.slot === "body"
          ? JSON.stringify(Schema.encodeUnknownSync(wire)(decoded))
          : JSON.stringify(Schema.encodeUnknownSync(typed)(decoded))
    } catch {
      skipped += 1
      continue
    }
    if (hasLoneSurrogate(expected)) {
      skipped += 1
      continue
    }
    seen.add(key)
    valid.push({ raw, expected })
    if (valid.length === 3) break
  }
  if (valid.length === 0) throw new Error(`requests: ${request.operationId} ${request.slot} produced no valid sample`)
  const invalid = invalidSamples(request.slot, request.schema, valid)
  return { valid, invalid, skipped }
}

function encodeRaw(slot: RequestRoot["slot"], schema: Schema.Top, sample: unknown): RequestRaw | undefined {
  let encoded: unknown
  try {
    encoded = Schema.encodeUnknownSync(schema as unknown as JsonCodec)(sample)
  } catch {
    return undefined
  }
  if (slot === "body") {
    if (hasLoneSurrogate(encoded)) return undefined
    return { body: JSON.stringify(encoded) }
  }
  if (hasLoneSurrogate(encoded)) return undefined
  if (!isObject(encoded)) return undefined
  if (slot === "params") {
    const entries = Object.entries(encoded).filter((entry): entry is [string, string] => typeof entry[1] === "string")
    if (entries.length === 0) return undefined
    return { params: Object.fromEntries(entries.map(([key, value]) => [key, encodeURIComponent(value)])) }
  }
  if (slot === "headers") {
    const entries = Object.entries(encoded).filter((entry): entry is [string, string] => typeof entry[1] === "string")
    return { headers: Object.fromEntries(entries) }
  }
  const pairs = Object.entries(encoded).flatMap(([key, value]) => {
    if (typeof value === "string") return [`${encodeURIComponent(key)}=${encodeURIComponent(value)}`]
    if (Array.isArray(value))
      return value
        .filter((item): item is string => typeof item === "string")
        .map((item) => `${encodeURIComponent(key)}=${encodeURIComponent(item)}`)
    return []
  })
  return { query: pairs.join("&") }
}

function decodeRaw(slot: RequestRoot["slot"], schema: Schema.Top, raw: RequestRaw): unknown {
  if (slot === "body") {
    const value = JSON.parse(raw.body ?? "")
    return Schema.decodeUnknownSync(Schema.toCodecJson(schema as unknown as JsonCodec))(value)
  }
  if (slot === "query") {
    const parsed = queryString.parse(raw.query ?? "")
    return Schema.decodeUnknownSync(schema as unknown as JsonCodec)(parsed)
  }
  const record = slot === "params" ? decodeParams(raw.params ?? {}) : (raw.headers ?? {})
  return Schema.decodeUnknownSync(schema as unknown as JsonCodec)(record)
}

function decodeParams(params: Record<string, string>) {
  return Object.fromEntries(Object.entries(params).map(([key, value]) => [key, decodeURIComponent(value)]))
}

function invalidSamples(slot: RequestRoot["slot"], schema: Schema.Top, valid: Array<RequestSample>): Array<RequestRaw> {
  const first = valid[0]!
  const invalid: Array<RequestRaw> = []
  const seen = new Set(valid.map((sample) => JSON.stringify(sample.raw)))
  const add = (raw: RequestRaw) => {
    const key = JSON.stringify(raw)
    if (seen.has(key)) return
    seen.add(key)
    try {
      decodeRaw(slot, schema, raw)
    } catch {
      invalid.push(raw)
    }
  }
  if (slot === "body") {
    const text = first.raw.body ?? ""
    add({ body: "{" })
    const value = JSON.parse(text)
    if (isObject(value)) {
      for (const key of Object.keys(value)) {
        const removed = { ...value }
        delete removed[key]
        add({ body: JSON.stringify(removed) })
      }
      for (const key of Object.keys(value)) add({ body: JSON.stringify({ ...value, [key]: "!!!invalid!!!" }) })
    }
    return invalid
  }
  if (slot === "query") {
    const pairs = (first.raw.query ?? "").split("&").filter((pair) => pair.length > 0)
    for (const pair of pairs) {
      add({ query: pairs.filter((candidate) => candidate !== pair).join("&") })
      add({ query: `${first.raw.query ?? ""}&${pair}` })
      const separator = pair.indexOf("=")
      add({
        query: pairs
          .map((candidate) => (candidate === pair ? `${pair.slice(0, separator + 1)}!!!` : candidate))
          .join("&"),
      })
    }
    add({ query: "location=x" })
    add({ query: `${first.raw.query ?? ""}&location=x` })
    return invalid
  }
  const record = slot === "params" ? (first.raw.params ?? {}) : (first.raw.headers ?? {})
  for (const key of Object.keys(record)) {
    const removed = { ...record }
    delete removed[key]
    if (slot === "params") add({ params: removed })
    else add({ headers: removed })
    const changed = { ...record, [key]: "!!!invalid!!!" }
    if (slot === "params") add({ params: changed })
    else add({ headers: changed })
  }
  return invalid
}
