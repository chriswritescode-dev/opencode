import { expect, test } from "bun:test"
import path from "node:path"
import { repository, runCommand, rustBinary, sandbox, startStdio, tsCommand } from "./support"

type Capture = {
  name: string
  status: number
  headers: Array<[string, string]>
  body: unknown
}

type CommandCapture = {
  name: string
  stdout: string
  exitCode: number
}

type Trace = {
  responses: Capture[]
  commands: CommandCapture[]
}

type Difference = {
  path: string
  typescript: unknown
  rust: unknown
}

const droppedHeaders = new Set(["date", "connection", "keep-alive", "content-length", "transfer-encoding"])
const commands = [
  ["service", "status"],
  ["service", "get"],
  ["service", "set", "port", "0"],
  ["service", "get", "port"],
]
const cookiePattern =
  /^opencode_session_\d+=(\d+\.[A-Za-z0-9_-]{43}); Max-Age=2592000; Path=\/; HttpOnly; SameSite=Lax$/

test("rust server behaves like the TypeScript oracle", async () => {
  await using box = await sandbox()
  const env = { ...box.env, OPENCODE_PASSWORD: "pw" }
  const pin = (await Bun.file(path.join(repository, "packages/server-rs/upstream-pin")).text()).trim()
  const pinCheck = Bun.spawnSync(
    ["git", "diff", "--quiet", pin, "HEAD", "--", "packages/protocol", "packages/server/src", "packages/cli/src"],
    { cwd: repository, stdout: "ignore", stderr: "ignore" },
  )
  if (pinCheck.exitCode !== 0) console.warn(`The checkout has moved past the embedded pin ${pin}.`)

  const typescript = await runTrace(tsCommand(), env)
  const rust = await runTrace([rustBinary()], env)

  expect(difference(typescript, rust)).toBeUndefined()
})

async function runTrace(command: readonly string[], env: Record<string, string>): Promise<Trace> {
  return { responses: await runResponses(command, env), commands: await runCommands(command, env) }
}

async function runResponses(command: readonly string[], env: Record<string, string>) {
  await using server = await startStdio(command, env)
  const credential = btoa("opencode:pw")
  const authorization = `Basic ${credential}`
  const pairInit: RequestInit = {
    method: "POST",
    headers: { authorization, "content-type": "application/json" },
    body: "{}",
  }
  const responses: Capture[] = []

  const exchange = async (name: string | undefined, route: string, init: RequestInit = {}) => {
    const response = await fetch(new URL(route, server.url), {
      redirect: "manual",
      signal: AbortSignal.timeout(10_000),
      ...init,
    })
    const text = await response.text()
    const capture: Capture = {
      name: name ?? "",
      status: response.status,
      headers: cleanHeaders(response.headers),
      body: parseBody(response.headers.get("content-type"), text),
    }
    if (name !== undefined) responses.push(capture)
    return capture
  }

  await exchange("info-basic", "/api/info", { headers: { authorization } })
  await exchange("info-token", `/api/info?auth_token=${encodeURIComponent(credential)}`)
  await exchange("info-none", "/api/info")
  await exchange("info-none-cors", "/api/info", { headers: { "sec-fetch-mode": "cors" } })
  await exchange("info-wrong", "/api/info", { headers: { authorization: `Basic ${btoa("opencode:wrong")}` } })
  await exchange("openapi-basic", "/openapi.json", { headers: { authorization } })
  await exchange("openapi-none", "/openapi.json")
  await exchange("openapi-gzip", "/openapi.json", { headers: { authorization, "accept-encoding": "gzip" } })

  const pair = await exchange("pair", "/api/pair", pairInit)
  const code = requireString(pair.body, "code")
  await exchange("connect-api", `/auth/connect/${encodeURIComponent(code)}`)
  await exchange("connect-api-again", `/auth/connect/${encodeURIComponent(code)}`)

  const freshPair = await exchange(undefined, "/api/pair", pairInit)
  const freshCode = requireString(freshPair.body, "code")
  const browser = await exchange("connect-browser", `/auth/connect/${encodeURIComponent(freshCode)}`, {
    headers: { accept: "text/html", "sec-fetch-mode": "navigate", "sec-fetch-dest": "document" },
  })
  const cookie = cookiePair(browser.headers)

  await exchange("info-cookie", "/api/info", { headers: { cookie } })
  await exchange("connect-bogus", "/auth/connect/bogus", { headers: { accept: "text/html" } })
  await exchange("options-info", "/api/info", {
    method: "OPTIONS",
    headers: { origin: "http://localhost:3000", "access-control-request-method": "GET" },
  })
  await exchange("cross-site", "/api/info", {
    headers: { authorization, "sec-fetch-site": "cross-site", origin: "https://evil.example" },
  })
  await exchange("api-nope", "/api/nope", { headers: { authorization } })

  return responses.map(normalizeCapture)
}

async function runCommands(command: readonly string[], env: Record<string, string>) {
  const results: CommandCapture[] = []
  for (const args of commands) {
    const result = await runCommand(command, args, env)
    results.push({
      name: args.join(" "),
      stdout: result.stdout.trim().replace(/http:\/\/127\.0\.0\.1:\d+/g, "<url>"),
      exitCode: result.exitCode,
    })
  }
  return results
}

function cleanHeaders(headers: Headers) {
  return Array.from(headers.entries())
    .map(([name, value]) => [name.toLowerCase(), value] as [string, string])
    .filter(([name]) => !droppedHeaders.has(name))
    .sort((left, right) =>
      left[0] === right[0] ? (left[1] < right[1] ? -1 : left[1] > right[1] ? 1 : 0) : left[0] < right[0] ? -1 : 1,
    )
}

function parseBody(contentType: string | null, text: string) {
  if (contentType?.includes("json")) {
    try {
      return JSON.parse(text) as unknown
    } catch {
      return text
    }
  }
  return text
}

function cookiePair(headers: Array<[string, string]>) {
  const header = headers.find(([name]) => name === "set-cookie")
  if (header === undefined) throw new Error("Browser pairing response has no set-cookie header")
  return header[1].split(";")[0]
}

function normalizeCapture(capture: Capture): Capture {
  return {
    name: capture.name,
    status: capture.status,
    headers:
      capture.name === "connect-browser"
        ? capture.headers.map(
            ([name, value]) => [name, name === "set-cookie" ? normalizeCookie(value) : value] as [string, string],
          )
        : capture.headers,
    body: normalizeBody(capture),
  }
}

function normalizeBody(capture: Capture) {
  if (capture.name.startsWith("openapi") && capture.status === 200) return summarizeOpenApi(capture.body)
  if (capture.name === "pair") return normalizePair(capture.body)
  if (capture.name === "connect-api") return normalizeSession(capture.body)
  if (capture.status === 200 && capture.name.startsWith("info")) return normalizeInfo(capture.body)
  return capture.body
}

function normalizeInfo(body: unknown) {
  if (!isRecord(body)) return body
  const normalized: Record<string, unknown> = { ...body, pid: "<pid>" }
  if (Array.isArray(body.urls)) {
    normalized.urls = body.urls.map((entry) =>
      typeof entry === "string" ? entry.replace(/^(https?:\/\/[^/]+?):\d+/, "$1:<port>") : entry,
    )
  }
  return normalized
}

function normalizePair(body: unknown) {
  if (!isRecord(body) || typeof body.code !== "string") return body
  expect(body.code).toMatch(/^[A-Za-z0-9_-]{22}$/)
  return { ...body, code: "<pairing-code>" }
}

function normalizeSession(body: unknown) {
  if (!isRecord(body) || typeof body.token !== "string") return body
  expect(body.token).toMatch(/^\d+\.[A-Za-z0-9_-]{43}$/)
  return { ...body, token: "<session-token>" }
}

function normalizeCookie(value: string) {
  expect(value).toMatch(cookiePattern)
  return value
    .replace(/^opencode_session_\d+=/, "opencode_session_<port>=")
    .replace(/\d+\.[A-Za-z0-9_-]{43}/, "<session-token>")
}

function summarizeOpenApi(body: unknown) {
  if (!isRecord(body)) return body
  expect(body.openapi).toBe("3.1.0")
  expect(isRecord(body.info) ? body.info.title : undefined).toBe("opencode HttpApi")
  const operations: string[] = []
  if (isRecord(body.paths)) {
    for (const item of Object.values(body.paths)) {
      if (!isRecord(item)) continue
      for (const operation of Object.values(item)) {
        if (isRecord(operation) && typeof operation.operationId === "string") operations.push(operation.operationId)
      }
    }
  }
  return { openapi: body.openapi, info: body.info, operations: operations.sort() }
}

function requireString(body: unknown, key: string) {
  if (!isRecord(body) || typeof body[key] !== "string") throw new Error(`Response is missing ${key}`)
  return body[key]
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

function difference(typescript: unknown, rust: unknown, at = ""): Difference | undefined {
  if (Object.is(typescript, rust)) return undefined
  if (Array.isArray(typescript) && Array.isArray(rust)) {
    for (let index = 0; index < Math.max(typescript.length, rust.length); index++) {
      const found = difference(typescript[index], rust[index], `${at}[${index}]`)
      if (found !== undefined) return found
    }
    return undefined
  }
  if (isRecord(typescript) && isRecord(rust)) {
    const keys = [...new Set([...Object.keys(typescript), ...Object.keys(rust)])].sort()
    for (const key of keys) {
      const field = at === "" ? key : `${at}.${key}`
      if (!(key in typescript) || !(key in rust)) {
        return { path: field, typescript: typescript[key], rust: rust[key] }
      }
      const found = difference(typescript[key], rust[key], field)
      if (found !== undefined) return found
    }
    return undefined
  }
  return { path: at, typescript, rust }
}
