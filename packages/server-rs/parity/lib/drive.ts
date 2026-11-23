import fs from "node:fs/promises"
import path from "node:path"
import { dumpDatabases, listFiles, waitForSearchSettled, type DatabaseCapture } from "./capture"
import type { ModelEndpoint } from "./model"
import { resolveSide, startServer, stopServer } from "./server"
import { sandboxEnv, type Sandbox } from "./sandbox"
import type { HttpStep, Scenario, Step, WaitEventStep } from "./scenario"

const credentials = Buffer.from("opencode:parity").toString("base64")
const droppedHeaders = new Set([
  "date",
  "connection",
  "keep-alive",
  "content-length",
  "transfer-encoding",
  "etag",
  "last-modified",
])
const placeholder = /\{\{([^{}]+)\}\}/g
const wholePlaceholder = /^\{\{([^{}]+)\}\}$/
const eventTimeout = 30_000

export type StepCapture = { status: number; headers: Record<string, string>; body: unknown }

export type Capture = {
  steps: StepCapture[]
  events: unknown[]
  logs: Record<string, unknown[]>
  model: unknown[]
  database: Record<string, DatabaseCapture>
  files: Record<string, string>
}

type Template = { sandbox: Sandbox; saved: Record<string, unknown> }
type Cursor = { index: number }
type StreamState = { ended: boolean; failure?: Error }

export async function runSide(
  scenario: Scenario,
  side: string,
  sandbox: Sandbox,
  endpoint: ModelEndpoint,
  extraEnv?: Record<string, string>,
): Promise<Capture> {
  const env = { ...(await sandboxEnv(sandbox.root, scenario.env, scenario.config, endpoint.url)), ...extraEnv }
  await writeFiles(sandbox, scenario.files ?? {})
  const server = await startServer(resolveSide(side), env, sandbox.work, side)
  const saved: Record<string, unknown> = {}
  const steps: StepCapture[] = []
  const events: unknown[] = []
  const logs: Record<string, unknown[]> = {}
  const cursor: Cursor = { index: 0 }
  let stream: EventStream | undefined
  let warmup: EventStream | undefined
  try {
    const warmupEvents: unknown[] = []
    warmup = await openEventStream(server.url, side, warmupEvents)
    const response = await fetch(new URL("/api/config", server.url), {
      headers: { authorization: `Basic ${credentials}`, "x-opencode-directory": sandbox.work },
      signal: AbortSignal.timeout(eventTimeout),
    })
    await response.body?.cancel()
    await waitForSearchSettled(sandbox.root)
    await settleEvents(warmupEvents)
    await warmup.close()
    warmup = undefined
    stream = await openEventStream(server.url, side, events)
    for (const step of scenario.steps) {
      const captured = await runStep(step, server.url, sandbox, saved, events, cursor)
      if (captured !== undefined) steps.push(captured)
    }
    await waitForSearchSettled(sandbox.root)
    await settleEvents(events)
    for (const id of sessionIDs(events)) logs[id] = await fetchSessionLog(server.url, id)
  } finally {
    const closing = Promise.all([stream?.close() ?? Promise.resolve(), warmup?.close() ?? Promise.resolve()])
    try {
      await stopServer(server)
    } finally {
      await closing
    }
  }
  const database = await dumpDatabases(sandbox.root)
  const files = await listFiles(sandbox.root)
  return { steps, events, logs, model: endpoint.requests(), database, files }
}

export function parseSseFrames(text: string): unknown[] {
  return text
    .replace(/\r\n/g, "\n")
    .split("\n\n")
    .flatMap((block) => {
      const data = block
        .split("\n")
        .filter((line) => line.startsWith("data:"))
        .map((line) => line.slice(5).replace(/^ /, ""))
      if (data.length === 0) return []
      const parsed = parseJSON(data.join("\n"))
      return parsed.ok ? [parsed.value] : []
    })
}

type EventStream = { close: () => Promise<void> }

async function openEventStream(base: string, side: string, events: unknown[]): Promise<EventStream> {
  const controller = new AbortController()
  const response = await fetch(new URL("/api/event", base), {
    headers: { authorization: `Basic ${credentials}` },
    signal: controller.signal,
  }).catch((error: unknown) => {
    throw new Error(`side ${side} event stream failed: ${message(error)}`)
  })
  if (!response.ok || response.body === null) {
    const status = response.status
    controller.abort()
    throw new Error(`side ${side} event stream failed: status ${status}`)
  }
  const state: StreamState = { ended: false }
  const loop = readEvents(response.body, (event) => events.push(event))
    .catch((error: unknown) => {
      if (!controller.signal.aborted) state.failure = error instanceof Error ? error : new Error(String(error))
    })
    .finally(() => {
      state.ended = true
    })
  try {
    await waitForFirstEvent(events, state, side)
  } catch (error) {
    controller.abort()
    await loop
    throw error
  }
  return {
    close: async () => {
      controller.abort()
      await loop
      if (state.failure !== undefined) throw state.failure
    },
  }
}

async function readEvents(body: ReadableStream<Uint8Array>, onEvent: (event: unknown) => void) {
  const reader = body.getReader()
  const decoder = new TextDecoder()
  let buffer = ""
  while (true) {
    const chunk = await reader.read()
    if (chunk.done) break
    buffer = (buffer + decoder.decode(chunk.value, { stream: true })).replace(/\r\n/g, "\n")
    const boundary = buffer.lastIndexOf("\n\n")
    if (boundary === -1) continue
    parseSseFrames(buffer.slice(0, boundary)).forEach(onEvent)
    buffer = buffer.slice(boundary + 2)
  }
  parseSseFrames(buffer + decoder.decode()).forEach(onEvent)
}

async function settleEvents(events: unknown[]) {
  const deadline = Date.now() + 5_000
  let seen = events.length
  let quietSince = Date.now()
  while (Date.now() < deadline) {
    await Bun.sleep(25)
    if (events.length !== seen) {
      seen = events.length
      quietSince = Date.now()
      continue
    }
    if (Date.now() - quietSince >= 1_000) return
  }
}

async function waitForFirstEvent(events: unknown[], state: StreamState, side: string) {
  const deadline = Date.now() + eventTimeout
  while (events.length === 0) {
    if (state.failure !== undefined) throw state.failure
    if (state.ended) throw new Error(`side ${side} event stream ended before the first event`)
    if (Date.now() > deadline) throw new Error(`side ${side} event stream produced no event within ${eventTimeout} ms`)
    await Bun.sleep(10)
  }
}

async function runStep(
  step: Step,
  base: string,
  sandbox: Sandbox,
  saved: Record<string, unknown>,
  events: unknown[],
  cursor: Cursor,
): Promise<StepCapture | undefined> {
  if ("files" in step) {
    await writeFiles(sandbox, step.files)
    return undefined
  }
  if ("waitEvent" in step) {
    cursor.index = await waitForEvent(
      step.waitEvent,
      events,
      cursor.index,
      step.timeout ?? eventTimeout,
      { sandbox, saved },
    )
    return undefined
  }
  return runHttpStep(step.http, base, sandbox, saved)
}

async function runHttpStep(step: HttpStep, base: string, sandbox: Sandbox, saved: Record<string, unknown>) {
  const template = { sandbox, saved }
  const url = new URL(base + interpolate(step.path, template))
  for (const [name, values] of Object.entries(step.query ?? {}))
    for (const value of Array.isArray(values) ? values : [values])
      url.searchParams.append(name, interpolate(value, template))
  const headers: Record<string, string> = {}
  const body = step.body === undefined ? undefined : JSON.stringify(templateBody(step.body, template))
  if (body !== undefined) headers["content-type"] = "application/json"
  for (const [name, value] of Object.entries(step.headers ?? {})) headers[name] = interpolate(value, template)
  const auth = step.auth ?? "basic"
  if (auth === "basic") headers.authorization = `Basic ${credentials}`
  if (auth === "query") url.searchParams.append("auth_token", credentials)
  if (step.location !== false) headers["x-opencode-directory"] = sandbox.work
  const response = await fetch(url, {
    method: step.method,
    headers,
    ...(body === undefined ? {} : { body }),
    signal: AbortSignal.timeout(30_000),
  })
  const content = response.headers.get("content-type") ?? ""
  const parsed = content.includes("json") ? await response.json() : await response.text()
  if (step.as !== undefined) saved[step.as] = parsed
  return { status: response.status, headers: captureHeaders(response.headers), body: parsed }
}

async function writeFiles(sandbox: Sandbox, files: Record<string, string>) {
  await Promise.all(
    Object.entries(files).map(async ([file, contents]) => {
      const target = path.join(sandbox.work, file)
      await fs.mkdir(path.dirname(target), { recursive: true })
      await Bun.write(target, contents)
    }),
  )
}

async function waitForEvent(
  step: WaitEventStep,
  events: unknown[],
  cursor: number,
  timeout: number,
  template: Template,
): Promise<number> {
  const match =
    step.match === undefined ? undefined : (templateBody(step.match, template) as Record<string, unknown>)
  const deadline = Date.now() + timeout
  while (true) {
    const found = findEvent(events, step.type, match, cursor)
    if (found !== undefined) return found + 1
    if (Date.now() > deadline) throw new Error(`scenario: timed out waiting for event ${step.type}`)
    await Bun.sleep(10)
  }
}

function findEvent(
  events: unknown[],
  type: string,
  match: Record<string, unknown> | undefined,
  cursor: number,
): number | undefined {
  for (let index = cursor; index < events.length; index += 1) {
    const event = events[index]
    if (!isRecord(event) || event.type !== type) continue
    if (match !== undefined && !deepContains(event.data, match)) continue
    return index
  }
  return undefined
}

function deepContains(target: unknown, expected: unknown): boolean {
  if (Array.isArray(expected)) {
    if (!Array.isArray(target) || target.length < expected.length) return false
    return expected.every((value, index) => deepContains(target[index], value))
  }
  if (isRecord(expected)) {
    if (!isRecord(target)) return false
    return Object.entries(expected).every(
      ([key, value]) => Object.hasOwn(target, key) && deepContains(target[key], value),
    )
  }
  return target === expected
}

function sessionIDs(events: unknown[]): string[] {
  const ids = new Set<string>()
  for (const event of events) {
    if (!isRecord(event) || event.type !== "session.created" || !isRecord(event.data)) continue
    const raw = event.data.id !== undefined ? event.data.id : event.data.sessionID
    if (typeof raw === "string") ids.add(raw)
  }
  return [...ids]
}

async function fetchSessionLog(base: string, sessionID: string): Promise<unknown[]> {
  const response = await fetch(new URL(`/api/experimental/session/${encodeURIComponent(sessionID)}/log`, base), {
    headers: { authorization: `Basic ${credentials}` },
    signal: AbortSignal.timeout(eventTimeout),
  })
  if (!response.ok) throw new Error(`scenario: session log failed for ${sessionID}: status ${response.status}`)
  return parseSseFrames(await response.text())
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

function parseJSON(text: string): { ok: true; value: unknown } | { ok: false } {
  try {
    return { ok: true, value: JSON.parse(text) }
  } catch {
    return { ok: false }
  }
}

function captureHeaders(headers: Headers) {
  const entries = [...headers].filter(([name]) => !droppedHeaders.has(name))
  entries.sort(([left], [right]) => (left < right ? -1 : left > right ? 1 : 0))
  return Object.fromEntries(entries)
}

function interpolate(text: string, template: Template) {
  return text.replace(placeholder, (_, name: string) => stringify(resolvePlaceholder(name, template)))
}

function templateBody(value: unknown, template: Template): unknown {
  if (typeof value === "string") {
    const whole = wholePlaceholder.exec(value)
    if (whole) return resolvePlaceholder(whole[1], template)
    return interpolate(value, template)
  }
  if (Array.isArray(value)) return value.map((entry) => templateBody(entry, template))
  if (value !== null && typeof value === "object")
    return Object.fromEntries(Object.entries(value).map(([key, entry]) => [key, templateBody(entry, template)]))
  return value
}

function resolvePlaceholder(name: string, template: Template): unknown {
  if (name === "work") return template.sandbox.work
  if (name === "root") return template.sandbox.root
  const [alias, ...segments] = name.split(".")
  if (segments.length === 0 || !(alias in template.saved)) throw unresolved(name)
  return segments.reduce<unknown>((value, segment) => {
    if (value === null || typeof value !== "object") throw unresolved(name)
    const next = Array.isArray(value) ? value[Number(segment)] : (value as Record<string, unknown>)[segment]
    if (next === undefined) throw unresolved(name)
    return next
  }, template.saved[alias])
}

function unresolved(name: string) {
  return new Error(`scenario: unresolved placeholder {{${name}}}`)
}

function stringify(value: unknown) {
  if (typeof value === "string") return value
  return JSON.stringify(value) ?? String(value)
}

function message(error: unknown) {
  return error instanceof Error ? error.message : String(error)
}
