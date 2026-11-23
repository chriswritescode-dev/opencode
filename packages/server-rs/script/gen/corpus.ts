import { mkdir } from "node:fs/promises"
import { Arbitrary, Effect, Schema } from "effect"
import { contractErrors } from "../contract/model"

const root = new URL("../..", import.meta.url).pathname
const file = `${root}crates/contract/tests/corpus/schemas.json`

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

const roots: Array<Root> = []
for (const error of errors) roots.push(await errorRoot(error.tag, error.schema))
roots.push(numbersRoot())
roots.sort((a, b) => (a.name < b.name ? -1 : 1))

const lines = roots.map(
  (item) => `${JSON.stringify(item.name)}:${JSON.stringify({ valid: item.valid, invalid: item.invalid })}`,
)
await mkdir(`${root}crates/contract/tests/corpus`, { recursive: true })
await Bun.write(file, `{\n${lines.join(",\n")}\n}\n`)

const total = (pick: (item: Root) => number) => roots.reduce((sum, item) => sum + pick(item), 0)
console.log(
  `corpus: ${roots.length} roots, ${total((item) => item.valid.length)} valid, ${total((item) => item.invalid.length)} invalid, ${total((item) => item.skipped)} skipped`,
)
for (const item of roots)
  console.log(`corpus: ${item.name} valid=${item.valid.length} invalid=${item.invalid.length} skipped=${item.skipped}`)

async function errorRoot(name: string, schema: Schema.Top): Promise<Root> {
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
    const input = JSON.stringify(encoded)
    valid.push({ input, expected: roundTrip(codec, input) })
  }
  if (valid.length === 0) throw new Error(`corpus: ${name} produced no surrogate-free sample`)
  const first = JSON.parse(valid[0].input)
  if (!isObject(first)) return { name, valid, invalid: [], skipped }

  const excess = { ...first, __excess__: 123 }
  const excessInput = JSON.stringify(excess)
  const decoded = Schema.decodeUnknownExit(codec)(excess)
  if (decoded._tag === "Success") {
    const expected = JSON.stringify(Schema.encodeUnknownSync(codec)(decoded.value))
    if (expected !== excessInput) valid.push({ input: excessInput, expected })
  }

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
  return JSON.stringify(Schema.encodeUnknownSync(codec)(Schema.decodeUnknownSync(codec)(JSON.parse(input))))
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
    return Object.values(value as Record<string, unknown>).some(hasLoneSurrogate)
  return false
}
