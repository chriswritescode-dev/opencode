import fs from "node:fs/promises"
import path from "node:path"
import { resolveSide, startServer, stopServer } from "./server"
import { sandboxEnv, type Sandbox } from "./sandbox"
import type { Scenario, Step } from "./scenario"

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

export type StepCapture = { status: number; headers: Record<string, string>; body: unknown }
export type Capture = { steps: StepCapture[] }

type Template = { sandbox: Sandbox; saved: Record<string, unknown> }

export async function runSide(
  scenario: Scenario,
  side: string,
  sandbox: Sandbox,
  extraEnv?: Record<string, string>,
): Promise<Capture> {
  const env = { ...(await sandboxEnv(sandbox.root, scenario.env, scenario.config)), ...extraEnv }
  for (const [file, contents] of Object.entries(scenario.files ?? {})) {
    const target = path.join(sandbox.work, file)
    await fs.mkdir(path.dirname(target), { recursive: true })
    await Bun.write(target, contents)
  }
  const server = await startServer(resolveSide(side), env, sandbox.work, side)
  const saved: Record<string, unknown> = {}
  const steps: StepCapture[] = []
  try {
    for (const step of scenario.steps) steps.push(await runStep(step, server.url, sandbox, saved))
  } finally {
    await stopServer(server)
  }
  return { steps }
}

async function runStep(step: Step, base: string, sandbox: Sandbox, saved: Record<string, unknown>) {
  const template = { sandbox, saved }
  const url = new URL(base + interpolate(step.http.path, template))
  for (const [name, values] of Object.entries(step.http.query ?? {}))
    for (const value of Array.isArray(values) ? values : [values])
      url.searchParams.append(name, interpolate(value, template))
  const headers: Record<string, string> = {}
  const body = step.http.body === undefined ? undefined : JSON.stringify(templateBody(step.http.body, template))
  if (body !== undefined) headers["content-type"] = "application/json"
  for (const [name, value] of Object.entries(step.http.headers ?? {})) headers[name] = interpolate(value, template)
  const auth = step.http.auth ?? "basic"
  if (auth === "basic") headers.authorization = `Basic ${credentials}`
  if (auth === "query") url.searchParams.append("auth_token", credentials)
  if (step.http.location !== false) headers["x-opencode-directory"] = sandbox.work
  const response = await fetch(url, {
    method: step.http.method,
    headers,
    ...(body === undefined ? {} : { body }),
    signal: AbortSignal.timeout(30_000),
  })
  const content = response.headers.get("content-type") ?? ""
  const parsed = content.includes("json") ? await response.json() : await response.text()
  if (step.http.as !== undefined) saved[step.http.as] = parsed
  return { status: response.status, headers: captureHeaders(response.headers), body: parsed }
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
