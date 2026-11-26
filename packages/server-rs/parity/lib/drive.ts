import fs from "node:fs/promises"
import path from "node:path"
import { dumpDatabases, listFiles, waitForSearchSettled, type DatabaseCapture } from "./capture"
import type { RequestRecord } from "./coverage"
import type { ModelEndpoint } from "./model"
import { resolveSide, startServer, stopServer } from "./server"
import { sandboxEnv, type Sandbox } from "./sandbox"
import type { GitStep, HttpStep, Scenario, Step, WaitEventStep, WsStep } from "./scenario"

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
export type WsCapture = {
  opened: boolean
  frames: string[]
  close: { code: number; reason: string } | null
  timedOut?: true
}

export type Capture = {
  steps: (StepCapture | WsCapture)[]
  events: unknown[]
  logs: Record<string, unknown[]>
  model: unknown[]
  database: Record<string, DatabaseCapture>
  files: Record<string, string>
}

type Template = { sandbox: Sandbox; saved: Record<string, unknown> }
type Cursor = { index: number }
type StreamState = { ended: boolean; failure?: Error }

const absentStream: StreamState = { ended: true }

export async function runSide(
  scenario: Scenario,
  side: string,
  sandbox: Sandbox,
  endpoint: ModelEndpoint,
  options?: { events?: boolean; env?: Record<string, string> },
): Promise<{ capture: Capture; requests: RequestRecord[] }> {
  const scenarioEnv = Object.fromEntries(
    Object.entries(scenario.env ?? {}).map(([name, value]) => [name, interpolate(value, { sandbox, saved: {} })]),
  )
  const env = { ...(await sandboxEnv(sandbox.root, scenarioEnv, scenario.config, endpoint.url)), ...options?.env }
  const captureEvents = options?.events ?? true
  await writeFiles(sandbox, scenario.files ?? {})
  for (const step of scenario.setup ?? []) {
    if ("files" in step) await writeFiles(sandbox, step.files)
    else await runGitStep(step.git, sandbox, {})
  }
  const server = await startServer(resolveSide(side), env, sandbox.work, side)
  const saved: Record<string, unknown> = {}
  const steps: (StepCapture | WsCapture)[] = []
  const requests: RequestRecord[] = []
  const events: unknown[] = []
  const logs: Record<string, unknown[]> = {}
  const cursor: Cursor = { index: 0 }
  let stream: EventStream | undefined
  let warmup: EventStream | undefined
  let succeeded = false
  try {
    const warmupEvents: unknown[] = []
    warmup = await openWarmupStream(server.url, side, warmupEvents, captureEvents)
    const response = await fetch(new URL("/api/config", server.url), {
      headers: { authorization: `Basic ${credentials}`, "x-opencode-directory": sandbox.work },
      signal: AbortSignal.timeout(eventTimeout),
    })
    await response.body?.cancel()
    await waitForSearchSettled(sandbox.root)
    if (captureEvents) {
      await settleEvents(warmupEvents)
      await warmup?.close()
      warmup = undefined
      stream = await openEventStream(server.url, side, events)
    } else if (warmup !== undefined) {
      await settleWarmup(warmup, warmupEvents)
      warmup = undefined
    }
    for (const step of scenario.steps) {
      const captured = await runStep(
        step,
        server.url,
        sandbox,
        saved,
        events,
        cursor,
        requests,
        stream?.state ?? absentStream,
      )
      if (captured !== undefined) steps.push(captured)
    }
    await waitForSearchSettled(sandbox.root)
    if (captureEvents) await settleEvents(events)
    for (const id of sessionIDs(events)) logs[id] = await fetchSessionLog(server.url, id)
    succeeded = true
  } finally {
    const closing = Promise.allSettled([stream?.close() ?? Promise.resolve(), warmup?.close() ?? Promise.resolve()])
    try {
      await stopServer(server)
    } finally {
      const closeFailure = (await closing).find((result) => result.status === "rejected")
      if (succeeded && closeFailure?.status === "rejected")
        throw closeFailure.reason instanceof Error ? closeFailure.reason : new Error(message(closeFailure.reason))
    }
  }
  const database = await dumpDatabases(sandbox.root)
  const files = await listFiles(sandbox.root)
  return { capture: { steps, events, logs, model: endpoint.requests(), database, files }, requests }
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
      const raw = data.join("\n")
      if (raw.length === 0) return []
      const parsed = parseJSON(raw)
      return parsed.ok ? [parsed.value] : [{ invalid: raw }]
    })
}

type EventStream = { close: () => Promise<void>; state: StreamState }

async function openEventStream(base: string, side: string, events: unknown[]): Promise<EventStream> {
  const controller = new AbortController()
  const timer = setTimeout(() => controller.abort(new Error(`no response within ${eventTimeout} ms`)), eventTimeout)
  const response = await fetch(new URL("/api/event", base), {
    headers: { authorization: `Basic ${credentials}` },
    signal: controller.signal,
  })
    .catch((error: unknown) => {
      throw new Error(`side ${side} event stream failed: ${message(error)}`)
    })
    .finally(() => clearTimeout(timer))
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
    state,
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
  throw new Error("scenario: event stream did not go quiet within 5 s")
}

async function openWarmupStream(base: string, side: string, events: unknown[], required: boolean) {
  try {
    return await openEventStream(base, side, events)
  } catch (error) {
    if (required) throw error
    return undefined
  }
}

async function settleWarmup(warmup: EventStream, events: unknown[]) {
  await settleEvents(events).catch(() => undefined)
  await warmup.close().catch(() => undefined)
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
  requests: RequestRecord[],
  state: StreamState,
): Promise<StepCapture | WsCapture | undefined> {
  if ("files" in step) {
    await writeFiles(sandbox, step.files)
    return undefined
  }
  if ("git" in step) {
    await runGitStep(step.git, sandbox, saved)
    return undefined
  }
  if ("ws" in step) return runWsStep(step.ws, base, sandbox, saved, requests)
  if ("waitEvent" in step) {
    cursor.index = await waitForEvent(
      step.waitEvent,
      events,
      cursor.index,
      step.timeout ?? eventTimeout,
      { sandbox, saved },
      state,
    )
    return undefined
  }
  return runHttpStep(step.http, base, sandbox, saved, requests)
}

async function runHttpStep(
  step: HttpStep,
  base: string,
  sandbox: Sandbox,
  saved: Record<string, unknown>,
  requests: RequestRecord[],
) {
  const template = { sandbox, saved }
  const url = new URL(base + interpolate(step.path, template))
  requests.push({ protocol: "http", method: step.method.toUpperCase(), path: url.pathname })
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
  const text = await response.text()
  const parsed = content.includes("json") ? parseBody(text) : text
  if (step.as !== undefined) saved[step.as] = parsed
  return { status: response.status, headers: captureHeaders(response.headers), body: parsed }
}

function parseBody(text: string): unknown {
  if (text.length === 0) return text
  const parsed = parseJSON(text)
  return parsed.ok ? parsed.value : text
}

function runWsStep(
  step: WsStep,
  base: string,
  sandbox: Sandbox,
  saved: Record<string, unknown>,
  requests: RequestRecord[],
): Promise<WsCapture> {
  const template = { sandbox, saved }
  const url = new URL(base + interpolate(step.path, template))
  requests.push({ protocol: "ws", method: "GET", path: url.pathname })
  for (const [name, values] of Object.entries(step.query ?? {}))
    for (const value of Array.isArray(values) ? values : [values])
      url.searchParams.append(name, interpolate(value, template))
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:"
  const headers: Record<string, string> = {}
  const auth = step.auth ?? "basic"
  if (auth === "basic") headers.authorization = `Basic ${credentials}`
  if (auth === "query") url.searchParams.append("auth_token", credentials)
  const messages = (step.send ?? []).map((message) => interpolate(message, template))
  return new Promise((resolve) => {
    const frames: string[] = []
    let opened = false
    let settled = false
    const socket = new WebSocket(url, { headers })
    socket.binaryType = "arraybuffer"
    const finish = (capture: WsCapture) => {
      if (settled) return
      settled = true
      clearTimeout(timer)
      if (step.as !== undefined) saved[step.as] = capture
      resolve(capture)
    }
    const timer = setTimeout(() => {
      socket.close(1000)
      finish({ opened, frames, close: null, timedOut: true })
    }, step.timeout ?? 10_000)
    socket.addEventListener("open", () => {
      opened = true
      for (const message of messages) socket.send(message)
      if (step.receive === 0) socket.close(1000)
    })
    socket.addEventListener("message", (event) => {
      if (settled || frames.length >= step.receive) return
      const data = event.data
      frames.push(typeof data === "string" ? data : `base64:${Buffer.from(data).toString("base64")}`)
      if (frames.length >= step.receive) socket.close(1000)
    })
    socket.addEventListener("close", (event) => {
      finish({ opened, frames, close: { code: event.code, reason: event.reason } })
    })
    socket.addEventListener("error", () => {
      if (!opened) finish({ opened: false, frames, close: null })
    })
  })
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

async function runGitStep(step: GitStep, sandbox: Sandbox, saved: Record<string, unknown>) {
  const template = { sandbox, saved }
  const args = step.args.map((arg) => interpolate(arg, template))
  const cwd = await resolveGitCwd(step.cwd === undefined ? undefined : interpolate(step.cwd, template), sandbox)
  const child = Bun.spawn(["git", ...args], {
    cwd,
    stdin: "ignore",
    stdout: "pipe",
    stderr: "pipe",
    env: {
      ...process.env,
      HOME: path.join(sandbox.root, "home"),
      GIT_CONFIG_NOSYSTEM: "1",
      GIT_ALLOW_PROTOCOL: "file",
      GIT_TERMINAL_PROMPT: "0",
      TZ: "UTC",
      GIT_AUTHOR_NAME: "Parity",
      GIT_AUTHOR_EMAIL: "parity@opencode.test",
      GIT_COMMITTER_NAME: "Parity",
      GIT_COMMITTER_EMAIL: "parity@opencode.test",
      GIT_AUTHOR_DATE: "2026-01-01T00:00:00Z",
      GIT_COMMITTER_DATE: "2026-01-01T00:00:00Z",
    },
  })
  const [exitCode, stderr] = await Promise.all([
    child.exited,
    new Response(child.stderr).text(),
    new Response(child.stdout).arrayBuffer(),
  ])
  if (exitCode !== 0)
    throw new Error(`scenario: git ${args.join(" ")} failed with exit ${exitCode}: ${stderr.trim()}`)
}

async function resolveGitCwd(cwd: string | undefined, sandbox: Sandbox) {
  const target = cwd === undefined ? sandbox.work : path.resolve(sandbox.work, cwd)
  const work = await fs.realpath(sandbox.work)
  const resolved = await fs.realpath(target).catch(() => path.resolve(target))
  if (resolved !== work && !resolved.startsWith(`${work}${path.sep}`))
    throw new Error(`scenario: git cwd is outside the sandbox work directory: ${cwd ?? target}`)
  return target
}

async function waitForEvent(
  step: WaitEventStep,
  events: unknown[],
  cursor: number,
  timeout: number,
  template: Template,
  state: StreamState,
): Promise<number> {
  const match = step.match === undefined ? undefined : (templateBody(step.match, template) as Record<string, unknown>)
  const deadline = Date.now() + timeout
  while (true) {
    const found = findEvent(events, step.type, match, cursor)
    if (found !== undefined) return found + 1
    if (state.failure !== undefined) throw new Error(`scenario: event stream failed: ${message(state.failure)}`)
    if (state.ended) throw new Error("scenario: event stream ended")
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
  const text = await response.text()
  if (!response.ok) return [{ status: response.status, body: text }]
  return parseSseFrames(text)
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
