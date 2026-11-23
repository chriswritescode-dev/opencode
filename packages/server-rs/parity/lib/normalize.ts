export type NormalizeContext = { root: string; start: number; end: number }

export type NormalizeState = { ids: Map<string, number>; counts: Map<string, number> }

export type Rule = {
  id: string
  description: string
  apply: (value: unknown, context: NormalizeContext, state: NormalizeState, key?: string) => unknown
}

const ID = /\b([a-z]{2,5})_[0-9A-Za-z]{26}\b/g
const LOOPBACK_IP = /127\.0\.0\.1:\d+/g
const LOOPBACK_HOST = /localhost:\d+/g
const ISO_TIME = /\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})/g
const DIGITS = /^\d+$/
const TIME_WINDOW = 3_600_000

export const RULES: Rule[] = [
  {
    id: "root",
    description: "Replace the capture root path with `<root>`.",
    apply: (value, context) => {
      if (typeof value !== "string" || context.root.length === 0) return value
      const encoded = encodeURIComponent(context.root)
      const replaced = value.split(context.root).join("<root>")
      if (encoded === context.root) return replaced
      return replaced.split(encoded).join("<root>")
    },
  },
  {
    id: "port",
    description: "Replace `127.0.0.1` and `localhost` ports with `<host>:<port>`.",
    apply: (value) => {
      if (typeof value !== "string") return value
      return value.replace(LOOPBACK_IP, "<host>:<port>").replace(LOOPBACK_HOST, "<host>:<port>")
    },
  },
  {
    id: "id",
    description: "Replace prefixed ids with stable `<prefix:n>` placeholders.",
    apply: (value, _context, state) => {
      if (typeof value !== "string") return value
      return value.replace(ID, (match, prefix: string) => {
        const known = state.ids.get(match)
        if (known !== undefined) return `<${prefix}:${known}>`
        const next = (state.counts.get(prefix) ?? 0) + 1
        state.counts.set(prefix, next)
        state.ids.set(match, next)
        return `<${prefix}:${next}>`
      })
    },
  },
  {
    id: "slug",
    description: "Replace session slugs with stable `<slug:n>` placeholders.",
    apply: (value, _context, state, key) => {
      if (key !== "slug" || typeof value !== "string") return value
      const lookup = `slug${"\u0000"}${value}`
      const known = state.ids.get(lookup)
      if (known !== undefined) return `<slug:${known}>`
      const next = (state.counts.get("slug") ?? 0) + 1
      state.counts.set("slug", next)
      state.ids.set(lookup, next)
      return `<slug:${next}>`
    },
  },
  {
    id: "time",
    description:
      "Replace timestamps inside the capture window with `<time>` for numbers, `<time:iso>` for ISO-8601 substrings and `<time:digits>` for digit strings.",
    apply: (value, context) => {
      const windowStart = context.start - TIME_WINDOW
      const windowEnd = context.end + TIME_WINDOW
      if (typeof value === "number") return value >= windowStart && value <= windowEnd ? "<time>" : value
      if (typeof value !== "string") return value
      const timed = value.replace(ISO_TIME, "<time:iso>")
      if (timed !== value) return timed
      if (!DIGITS.test(value)) return value
      const parsed = Number(value)
      return parsed >= windowStart && parsed <= windowEnd ? "<time:digits>" : value
    },
  },
  {
    id: "process",
    description: "Replace numeric `pid` and string `hostname` values with placeholders.",
    apply: (value, _context, _state, key) => {
      if (key === "pid" && typeof value === "number") return "<pid>"
      if (key === "hostname" && typeof value === "string") return "<hostname>"
      return value
    },
  },
  {
    id: "base64",
    description: "Decode base64 JSON values, normalize them and re-encode the result.",
    apply: (value, context, state) => {
      if (typeof value !== "string") return value
      const decoded = decodeBase64Json(value)
      if (decoded === undefined) return value
      return Buffer.from(JSON.stringify(walk(decoded, undefined, context, state))).toString("base64")
    },
  },
]

const BASE64 = /^[A-Za-z0-9+/]+={0,2}$/

const decodeBase64Json = (value: string): unknown => {
  if (value.length < 8 || !BASE64.test(value)) return undefined
  const text = Buffer.from(value, "base64").toString("utf-8")
  if (!text.startsWith("{") && !text.startsWith("[")) return undefined
  const parsed = parseJSON(text)
  return parsed.ok ? parsed.value : undefined
}

const isPlainObject = (value: unknown): value is Record<string, unknown> => {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return false
  const prototype = Object.getPrototypeOf(value)
  return prototype === Object.prototype || prototype === null
}

const parseJSON = (text: string): { ok: true; value: unknown } | { ok: false } => {
  try {
    return { ok: true, value: JSON.parse(text) }
  } catch {
    return { ok: false }
  }
}

const applyRules = (value: unknown, key: string | undefined, context: NormalizeContext, state: NormalizeState) =>
  RULES.reduce((current, rule) => rule.apply(current, context, state, key), value)

const INDENTATIONS: (string | number | undefined)[] = [undefined, 2, "\t", 4]

const stringifyJSON = (value: unknown, indentation: string | number | undefined) =>
  indentation === undefined ? JSON.stringify(value) : JSON.stringify(value, null, indentation)

const matchJSONFormatting = (raw: string, value: unknown) => {
  const text = raw.trimEnd()
  const suffix = raw.slice(text.length)
  const formatting = INDENTATIONS.map((indentation) => ({ indentation, text: stringifyJSON(value, indentation) })).find(
    (candidate) => candidate.text === text,
  )
  if (formatting === undefined) return undefined
  return { indentation: formatting.indentation, suffix }
}

const normalizeJSONText = (raw: string, context: NormalizeContext, state: NormalizeState) => {
  const parsed = parseJSON(raw)
  if (!parsed.ok) return undefined
  const formatting = matchJSONFormatting(raw, parsed.value)
  if (formatting === undefined) return undefined
  const text = stringifyJSON(walk(parsed.value, undefined, context, state), formatting.indentation)
  if (text === undefined) return undefined
  return text + formatting.suffix
}

const walk = (value: unknown, key: string | undefined, context: NormalizeContext, state: NormalizeState): unknown => {
  if (Array.isArray(value)) return value.map((element) => walk(element, undefined, context, state))
  if (isPlainObject(value))
    return Object.fromEntries(
      Object.keys(value)
        .sort()
        .map((entryKey) => [
          String(applyRules(entryKey, undefined, context, state)),
          walk(value[entryKey], entryKey, context, state),
        ]),
    )
  if (typeof value === "string" && (value.startsWith("{") || value.startsWith("["))) {
    const normalized = normalizeJSONText(value, context, state)
    if (normalized !== undefined) return normalized
  }
  return applyRules(value, key, context, state)
}

export function normalizeCapture(capture: unknown, context: NormalizeContext): unknown {
  const state: NormalizeState = { ids: new Map(), counts: new Map() }
  const walked = isPlainObject(capture)
    ? Object.fromEntries(Object.entries(capture).map(([key, value]) => [key, walk(value, key, context, state)]))
    : walk(capture, undefined, context, state)
  if (!isPlainObject(walked) || !isPlainObject(walked.database)) return walked
  const database = Object.fromEntries(
    Object.entries(walked.database).map(([name, entry]) => {
      if (!isPlainObject(entry) || !isPlainObject(entry.tables)) return [name, entry]
      const tables = Object.fromEntries(
        Object.entries(entry.tables).map(([table, rows]) => [
          table,
          Array.isArray(rows) ? [...rows].sort(compareRows) : rows,
        ]),
      )
      return [name, { ...entry, tables }]
    }),
  )
  return { ...walked, database }
}

const compareRows = (left: unknown, right: unknown) => {
  const leftText = JSON.stringify(left)
  const rightText = JSON.stringify(right)
  if (leftText === rightText) return 0
  return leftText < rightText ? -1 : 1
}
