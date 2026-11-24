import { constants } from "node:os"
import path from "node:path"

const repo = path.resolve(import.meta.dir, "../../../..")

const children = new Set<Bun.Subprocess<"pipe", "pipe", "pipe">>()

const stopAll = () => children.forEach((child) => child.kill("SIGKILL"))

process.on("exit", stopAll)

export function stopServersOnSignal() {
  for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"] as const)
    process.on(signal, () => {
      stopAll()
      process.exit(128 + constants.signals[signal])
    })
}

export type RunningServer = { url: string; process: Bun.Subprocess<"pipe", "pipe", "pipe"> }

export function resolveSide(spec: string): string[] {
  if (spec !== "ts") return [path.resolve(spec)]
  const cli = path.join(repo, "packages/cli")
  return [
    "bun",
    "--no-install",
    `--preload=${Bun.resolveSync("@opentui/solid/preload", cli)}`,
    path.join(cli, "src/index.ts"),
  ]
}

export async function startServer(
  command: string[],
  env: Record<string, string>,
  cwd: string,
  label: string,
): Promise<RunningServer> {
  const child = Bun.spawn([...command, "serve", "--stdio", "--port", "0"], {
    cwd,
    env,
    stdin: "pipe",
    stdout: "pipe",
    stderr: "pipe",
  })
  children.add(child)
  child.exited.then(() => children.delete(child))
  const stderr = watch(child.stderr)
  const timeout = Number(process.env.PARITY_START_TIMEOUT) || 60_000
  const started = readFirstLine(child.stdout).then(
    (line) => ({ kind: "line" as const, line }),
    (error: unknown) => ({ kind: "error" as const, reason: message(error) }),
  )
  let timer: ReturnType<typeof setTimeout> | undefined
  const outcome = await Promise.race([
    started,
    new Promise<{ kind: "timeout" }>((resolve) => {
      timer = setTimeout(() => resolve({ kind: "timeout" }), timeout)
    }),
    child.exited.then((code) => ({ kind: "exit" as const, code })),
  ])
  clearTimeout(timer)
  const fail = async (reason: string): Promise<never> => {
    child.kill("SIGKILL")
    children.delete(child)
    await Promise.race([stderr.done, Bun.sleep(250)])
    const detail = stderr.text().trim().slice(-2000)
    throw new Error(`side ${label} did not start: ${reason}` + (detail ? `\n${detail}` : ""))
  }
  if (outcome.kind === "timeout") return fail(`timed out after ${timeout} ms`)
  if (outcome.kind === "exit") return fail(`exited with code ${outcome.code}`)
  if (outcome.kind === "error") {
    const code = await Promise.race([child.exited, Bun.sleep(50).then(() => undefined)])
    return fail(code === undefined ? outcome.reason : `exited with code ${code}`)
  }
  const url = parseUrl(outcome.line)
  if (url === undefined) return fail(`malformed first line: ${outcome.line.slice(0, 200)}`)
  return { url, process: child }
}

export async function stopServer(server: RunningServer): Promise<void> {
  try {
    await server.process.stdin?.end()
  } catch {}
  if (await exitsWithin(server.process.exited, 10_000)) return
  server.process.kill("SIGTERM")
  if (await exitsWithin(server.process.exited, 5_000)) return
  server.process.kill("SIGKILL")
  await server.process.exited
}

function parseUrl(line: string) {
  try {
    const value = JSON.parse(line) as { url?: unknown }
    if (typeof value?.url === "string") return value.url
  } catch {}
  return undefined
}

function message(error: unknown) {
  return error instanceof Error ? error.message : String(error)
}

async function readFirstLine(stream: ReadableStream) {
  const decoder = new TextDecoder()
  const reader = stream.getReader()
  let buffer = ""
  while (true) {
    const chunk = await reader.read()
    if (chunk.done) throw new Error("exited before a line")
    buffer += decoder.decode(chunk.value, { stream: true })
    const end = buffer.indexOf("\n")
    if (end !== -1) {
      void drain(reader).catch(() => {})
      return buffer.slice(0, end)
    }
  }
}

function watch(stream: ReadableStream) {
  const decoder = new TextDecoder()
  const reader = stream.getReader()
  let text = ""
  const done = (async () => {
    while (true) {
      const chunk = await reader.read()
      if (chunk.done) {
        text += decoder.decode()
        return
      }
      text += decoder.decode(chunk.value, { stream: true })
    }
  })().catch(() => {})
  return { text: () => text, done }
}

async function drain(reader: { read(): Promise<{ done: boolean }> }) {
  while (!(await reader.read()).done) {}
}

async function exitsWithin(exited: Promise<unknown>, ms: number) {
  let timer: ReturnType<typeof setTimeout> | undefined
  const timeout = new Promise<boolean>((resolve) => {
    timer = setTimeout(() => resolve(false), ms)
  })
  const finished = await Promise.race([exited.then(() => true), timeout])
  clearTimeout(timer)
  return finished
}
