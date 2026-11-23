import type { Connection, Context, Outcome } from "./connection"
import type { JsonValue } from "./protocol.gen"

const fs = process.getBuiltinModule("node:fs")
const retained: Uint8Array[] = []
let aborts = 0

export function register(
  connection: Connection,
  options: { test: boolean; maxFrame: number; write: (bytes: Uint8Array) => void },
): void {
  connection.register("host.echo", echo)
  connection.register("host.memory", memory)
  if (!options.test) return
  connection.register("host.sleep", sleep)
  connection.register("host.aborts", abortsHandler)
  connection.register("host.nested", nested)
  connection.register("host.location", location)
  connection.registerStream("host.stream", stream)
  connection.register("host.streamStats", async () => ({
    value: { maxInflight: connection.stats.maxInflight },
  }))
  connection.register("host.consume", (ctx, params) => consume(connection, ctx, params))
  connection.register("host.abort", abortCall)
  connection.register("host.crash", crash)
  connection.register("host.oversize", async () => {
    const header = Buffer.alloc(5)
    header.writeUInt32LE(options.maxFrame + 1, 0)
    header[4] = 0
    options.write(header)
    return { value: null }
  })
  connection.register("host.block", block)
  connection.register("host.allocate", allocate)
  connection.register("host.env", async () => ({ value: Object.keys(process.env).sort() }))
  connection.register("host.print", print)
  connection.register("host.import", importModule)
  connection.register("host.children", children)
}

async function echo(_ctx: Context, params: JsonValue, bytes?: Uint8Array): Promise<Outcome> {
  return { value: params, bytes }
}

async function memory(): Promise<Outcome> {
  const usage = process.memoryUsage()
  const rss = usage.rss
  const heapTotal = usage.heapTotal
  const heapUsed = usage.heapUsed
  const external = usage.external
  return { value: { rss, heapTotal, heapUsed, external } }
}

function sleep(ctx: Context, params: JsonValue): Promise<Outcome> {
  const ms = numberAt(params, "ms", 0)
  return new Promise((resolve, reject) => {
    let timer: ReturnType<typeof setTimeout> | undefined
    const finish = () => {
      ctx.signal.removeEventListener("abort", abort)
      clearTimeout(timer)
    }
    const abort = () => {
      finish()
      aborts += 1
      const error = new Error(`host.sleep aborted after ${ms}ms`)
      error.name = "Aborted"
      reject(error)
    }
    if (ctx.signal.aborted) {
      abort()
      return
    }
    timer = setTimeout(() => {
      finish()
      resolve({ value: { ms } })
    }, ms)
    ctx.signal.addEventListener("abort", abort)
  })
}

async function abortsHandler(): Promise<Outcome> {
  return { value: aborts }
}

async function nested(ctx: Context, params: JsonValue): Promise<Outcome> {
  const method = stringAt(params, "method", "")
  const inner = isRecord(params) ? (params.params ?? null) : null
  return ctx.call(method, inner)
}

async function location(ctx: Context): Promise<Outcome> {
  return { value: ctx.location ?? null }
}

async function* stream(ctx: Context, params: JsonValue): AsyncIterable<Outcome> {
  const count = numberAt(params, "count", 0)
  const size = numberAt(params, "size", 0)
  for (let index = 0; index < count; index++) {
    if (ctx.signal.aborted) return
    const chunk = size > 0 ? { value: { index }, bytes: new Uint8Array(size) } : { value: { index } }
    yield chunk
  }
}

async function consume(connection: Connection, ctx: Context, params: JsonValue): Promise<Outcome> {
  const method = stringAt(params, "method", "")
  const inner = isRecord(params) ? (params.params ?? null) : null
  const delayMs = numberAt(params, "delayMs", 0)
  let chunks = 0
  for await (const _chunk of ctx.stream(method, inner)) {
    chunks += 1
    if (delayMs > 0) await new Promise((resolve) => setTimeout(resolve, delayMs))
  }
  return { value: { count: chunks, maxBuffered: connection.stats.maxBuffered } }
}

async function abortCall(ctx: Context, params: JsonValue): Promise<Outcome> {
  const method = stringAt(params, "method", "")
  const inner = isRecord(params) ? (params.params ?? null) : null
  const ms = numberAt(params, "ms", 0)
  const controller = new AbortController()
  const timer = setTimeout(() => controller.abort(), ms)
  try {
    await ctx.call(method, inner, { signal: controller.signal })
    return { value: { aborted: false } }
  } catch (error) {
    return { value: { aborted: error instanceof Error && error.name === "AbortError" } }
  } finally {
    clearTimeout(timer)
  }
}

async function crash(_ctx: Context, params: JsonValue): Promise<Outcome> {
  process.exit(numberAt(params, "code", 0))
}

async function block(_ctx: Context, params: JsonValue): Promise<Outcome> {
  const ms = numberAt(params, "ms", 0)
  const start = performance.now()
  while (performance.now() - start < ms) {
    continue
  }
  return { value: { ms } }
}

async function allocate(_ctx: Context, params: JsonValue): Promise<Outcome> {
  const mb = numberAt(params, "mb", 0)
  retained.push(new Uint8Array(mb * 1024 * 1024).fill(1))
  return { value: { mb } }
}

async function print(_ctx: Context, params: JsonValue): Promise<Outcome> {
  const text = stringAt(params, "text", "")
  fs.writeSync(1, `${text}:stdout\n`)
  console.error(`${text}:stderr`)
  return { value: null }
}

async function importModule(_ctx: Context, params: JsonValue): Promise<Outcome> {
  const specifier = stringAt(params, "path", "")
  const module = await import(specifier)
  return { value: Object.keys(module).sort() }
}

async function children(): Promise<Outcome> {
  const child = Bun.spawn(["sleep", "60"])
  return { value: { pid: child.pid } }
}

function isRecord(value: JsonValue): value is { [key in string]: JsonValue } {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

function isNumber(value: JsonValue | undefined): value is number {
  return typeof value === "number"
}

function isString(value: JsonValue | undefined): value is string {
  return typeof value === "string"
}

function numberAt(params: JsonValue, name: string, fallback: number): number {
  if (!isRecord(params)) return fallback
  const value = params[name]
  return isNumber(value) ? value : fallback
}

function stringAt(params: JsonValue, name: string, fallback: string): string {
  if (!isRecord(params)) return fallback
  const value = params[name]
  return isString(value) ? value : fallback
}
