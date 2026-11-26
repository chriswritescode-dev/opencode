import { expect, test } from "bun:test"
import { existsSync } from "node:fs"
import path from "node:path"
import { startModelEndpoint, type ModelEndpoint } from "../../parity/lib/model"
import { createSandbox, sandboxEnv, type Sandbox } from "../../parity/lib/sandbox"
import { rustBinary, tsCommand } from "./support"

type Running = { url: string; process: Bun.Subprocess<"pipe" | "pipe" | "pipe">; output: () => string }

const cassette =
  "packages/ai/test/fixtures/recordings/anthropic-compatible-messages/minimax-m3-anthropic-compatible-text.json"

const config = {
  model: "anthropic-test/test-model",
  agents: { build: { permissions: [{ action: "*", resource: "*", effect: "deny" }] } },
}

const nativeBinary = path.resolve(import.meta.dir, "../../../cli-rs/target/debug/opencode-native")

type Harness = { box: Sandbox; endpoint: ModelEndpoint; env: Record<string, string> }

async function harness(): Promise<Harness> {
  const box = await createSandbox()
  const endpoint = await startModelEndpoint()
  const env = await sandboxEnv(box.root, {}, config, endpoint.url)
  return { box, endpoint, env }
}

async function disposeHarness(value: Harness) {
  await value.endpoint.stop()
  await removeSandbox(value.box.root)
}

function loadReply(endpoint: ModelEndpoint, box: Sandbox) {
  endpoint.load([{ cassette }], { work: box.work, root: box.root })
}

async function startStdio(env: Record<string, string>, cwd: string): Promise<Running> {
  const child = Bun.spawn([rustBinary(), "serve", "--stdio", "--port", "0"], {
    cwd,
    env,
    stdin: "pipe",
    stdout: "pipe",
    stderr: "pipe",
  })
  const line = await firstLine(child, 60_000)
  return { url: parseJsonUrl(line), process: child, output: watcher(child) }
}

async function startPort(env: Record<string, string>, cwd: string): Promise<Running> {
  const child = Bun.spawn([rustBinary(), "serve", "--port", "0"], {
    cwd,
    env,
    stdin: "pipe",
    stdout: "pipe",
    stderr: "pipe",
  })
  const line = await firstLine(child, 60_000)
  const url = /listening on (http:\/\/\S+)/.exec(line)?.[1]
  if (url === undefined) throw new Error(`unexpected server line: ${line}`)
  return { url, process: child, output: watcher(child) }
}

async function stopStdio(server: Running) {
  await server.process.stdin?.end()
  if (await exits(server.process, 10_000)) return
  server.process.kill("SIGTERM")
  if (await exits(server.process, 5_000)) return
  server.process.kill("SIGKILL")
  await server.process.exited
}

async function stopPort(server: Running) {
  server.process.kill("SIGTERM")
  if (await exits(server.process, 10_000)) return
  server.process.kill("SIGKILL")
  await server.process.exited
}

async function firstLine(child: Bun.Subprocess<"pipe", "pipe", "pipe">, timeout: number) {
  const stdout = child.stdout
  if (typeof stdout === "number" || stdout === undefined) throw new Error("server stdout is not piped")
  const reader = stdout.getReader()
  const decoder = new TextDecoder()
  const started = Date.now()
  let buffer = ""
  while (true) {
    const chunk = await Promise.race([
      reader.read(),
      Bun.sleep(Math.max(0, timeout - (Date.now() - started))).then(() => undefined),
    ])
    if (chunk === undefined) {
      child.kill("SIGKILL")
      throw new Error(`server did not start within ${timeout} ms`)
    }
    if (chunk.done) throw new Error(`server exited before printing a URL: ${buffer}`)
    buffer += decoder.decode(chunk.value, { stream: true })
    const end = buffer.indexOf("\n")
    if (end !== -1) {
      void (async () => {
        while (!(await reader.read()).done) {}
      })().catch(() => {})
      return buffer.slice(0, end)
    }
  }
}

function watcher(child: Bun.Subprocess<"pipe", "pipe", "pipe">) {
  let stderr = ""
  if (typeof child.stderr === "number" || child.stderr === undefined) return () => stderr
  void new Response(child.stderr)
    .text()
    .then((text) => {
      stderr = text
    })
    .catch(() => {})
  return () => stderr
}

function parseJsonUrl(line: string) {
  const value = JSON.parse(line) as { url?: unknown }
  if (typeof value.url !== "string") throw new Error(`unexpected server line: ${line}`)
  return value.url
}

function exits(process: Bun.Subprocess<"pipe" | "pipe" | "pipe">, milliseconds: number) {
  return Promise.race([process.exited.then(() => true), Bun.sleep(milliseconds).then(() => false)])
}

function api(url: string, directory: string) {
  const authorization = `Basic ${Buffer.from("opencode:parity").toString("base64")}`
  return async (method: string, pathname: string, body?: unknown): Promise<any> => {
    const headers: Record<string, string> = { authorization, "x-opencode-directory": directory }
    if (body !== undefined) headers["content-type"] = "application/json"
    const response = await fetch(new URL(pathname, url), {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: AbortSignal.timeout(30_000),
    })
    const text = await response.text()
    if (!response.ok) throw new Error(`${method} ${pathname} answered ${response.status}: ${text}`)
    return text === "" ? undefined : JSON.parse(text)
  }
}

function assistantText(messages: unknown) {
  if (!isRecord(messages) || !Array.isArray(messages.data)) return undefined
  for (const message of messages.data) {
    if (!isRecord(message) || message.type !== "assistant" || !Array.isArray(message.content)) continue
    for (const part of message.content) {
      if (isRecord(part) && part.type === "text" && typeof part.text === "string") return part.text
    }
  }
  return undefined
}

function isRecord(value: unknown): value is Record<string, any> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

async function conversation(box: Sandbox, server: Running) {
  const request = api(server.url, box.work)
  const created = await request("POST", "/api/session", {})
  const id = created.data.id as string
  await request("POST", `/api/session/${encodeURIComponent(id)}/prompt`, { text: "say hi" })
  await request("POST", `/api/experimental/session/${encodeURIComponent(id)}/wait`)
  return { request, id }
}

test("restart keeps the first turn in the database", async () => {
  const value = await harness()
  let server: Running | undefined
  try {
    loadReply(value.endpoint, value.box)
    server = await startStdio(value.env, value.box.work)
    const { request, id } = await conversation(value.box, server)
    const before = await request("GET", `/api/session/${encodeURIComponent(id)}/message`)
    expect(assistantText(before)).toBe("Hello!")
    await stopStdio(server)
    server = undefined

    server = await startStdio(value.env, value.box.work)
    const restarted = api(server.url, value.box.work)
    const messages = await restarted("GET", `/api/session/${encodeURIComponent(id)}/message`)
    const user = (messages.data as any[]).find((message) => message.type === "user")
    expect(user?.text).toBe("say hi")
    expect(assistantText(messages)).toBe("Hello!")
  } finally {
    if (server !== undefined) await stopStdio(server).catch(() => {})
    await disposeHarness(value)
  }
})

test("the TypeScript run command completes a turn", async () => {
  const value = await harness()
  let server: Running | undefined
  try {
    loadReply(value.endpoint, value.box)
    server = await startPort(value.env, value.box.work)
    const cli = await run(tsCommand(), ["run", "--server", server.url, "say hi"], value.env, value.box.work)
    if (cli.exitCode !== 0 || cli.stderr !== "")
      throw new Error(`run failed with ${cli.exitCode}: ${cli.stderr}\nserver: ${server.output()}`)
    expect(cli.stdout).toContain("Hello!")
    expect(value.endpoint.unused()).toBe(0)
    expect(value.endpoint.failures()).toEqual([])
  } finally {
    if (server !== undefined) await stopPort(server).catch(() => {})
    await disposeHarness(value)
  }
})

test("the native run command completes a turn", async () => {
  if (!existsSync(nativeBinary)) {
    throw new Error(
      `Missing ${nativeBinary}; build it with bun run --cwd packages/cli-rs cargo build -q --bin opencode-native`,
    )
  }
  const value = await harness()
  try {
    loadReply(value.endpoint, value.box)
    const env = { ...value.env, OPENCODE_TS_BIN: rustBinary() }
    const cli = await run([nativeBinary], ["run", "--standalone", "say hi"], env, value.box.work)
    if (cli.exitCode !== 0 || cli.stderr !== "") throw new Error(`run failed with ${cli.exitCode}: ${cli.stderr}`)
    expect(cli.stdout).toContain("Hello!")
    expect(value.endpoint.unused()).toBe(0)
    expect(value.endpoint.failures()).toEqual([])
  } finally {
    await disposeHarness(value)
  }
})

test("the TypeScript TUI completes a turn", async () => {
  const value = await harness()
  let server: Running | undefined
  let tui: Tui | undefined
  try {
    loadReply(value.endpoint, value.box)
    server = await startPort(value.env, value.box.work)
    tui = await startTui(server.url, value.env, value.box.work)
    await waitForAssistant(api(server.url, value.box.work), 90_000)
    expect(tui.output()).not.toContain("panic")
  } finally {
    await tui?.stop()
    if (server !== undefined) await stopPort(server).catch(() => {})
    await disposeHarness(value)
  }
})

type Tui = { process: Bun.Subprocess<"ignore" | "pipe" | "pipe">; output: () => string; stop: () => Promise<void> }

async function startTui(url: string, env: Record<string, string>, cwd: string): Promise<Tui> {
  const command = `${[...tsCommand(), "--server", url, "--prompt", "say hi"].map(shellQuote).join(" ")}`
  const child = Bun.spawn(["script", "-qfec", command, "/dev/null"], {
    cwd,
    env,
    stdin: "ignore",
    stdout: "pipe",
    stderr: "pipe",
    detached: true,
  })
  let output = ""
  const collect = async (stream: ReadableStream<Uint8Array>) => {
    const decoder = new TextDecoder()
    for await (const chunk of stream) output += decoder.decode(chunk, { stream: true })
  }
  void collect(child.stdout).catch(() => {})
  void collect(child.stderr).catch(() => {})
  return {
    process: child as Tui["process"],
    output: () => output,
    stop: async () => {
      try {
        process.kill(-child.pid, "SIGKILL")
      } catch {}
      await Promise.race([child.exited, Bun.sleep(5_000)])
    },
  }
}

async function waitForAssistant(request: (method: string, pathname: string) => Promise<any>, timeout: number) {
  const deadline = Date.now() + timeout
  while (Date.now() < deadline) {
    const list = await request("GET", "/api/session").catch(() => undefined)
    if (Array.isArray(list?.data)) {
      for (const session of list.data) {
        if (typeof session?.id !== "string") continue
        const messages = await request("GET", `/api/session/${encodeURIComponent(session.id)}/message`).catch(
          () => undefined,
        )
        if (assistantText(messages) === "Hello!") return
      }
    }
    await Bun.sleep(250)
  }
  throw new Error(`the TUI did not produce Hello! within ${timeout} ms`)
}

function shellQuote(value: string) {
  return `'${value.replaceAll("'", `'\\''`)}'`
}

async function run(command: readonly string[], args: readonly string[], env: Record<string, string>, cwd: string) {
  const child = Bun.spawn([...command, ...args], { cwd, env, stdin: "ignore", stdout: "pipe", stderr: "pipe" })
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
    child.exited,
  ])
  return { stdout, stderr, exitCode }
}

function removeSandbox(root: string) {
  return import("node:fs/promises").then((fs) => fs.rm(root, { recursive: true, force: true, maxRetries: 10 }))
}
