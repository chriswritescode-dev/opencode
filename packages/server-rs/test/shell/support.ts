import { existsSync } from "node:fs"
import fs from "node:fs/promises"
import net from "node:net"
import os from "node:os"
import path from "node:path"

export const repository = path.resolve(import.meta.dir, "../../../..")
export const cliDirectory = path.join(repository, "packages/cli")

export function rustBinary() {
  const binary = process.env.OPENCODE_SERVER_BIN ?? path.join(repository, "packages/server-rs/target/debug/opencode-server")
  if (!existsSync(binary)) throw new Error(`Missing ${binary}; build it with bun run cargo build -p opencode-server`)
  return binary
}

export function tsCommand() {
  const preload = Bun.resolveSync("@opentui/solid/preload", cliDirectory)
  return ["bun", `--preload=${preload}`, path.join(cliDirectory, "src/index.ts")]
}

export async function sandbox() {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "opencode-server-shell-"))
  const home = path.join(root, "home")
  const cache = path.join(root, "cache")
  const config = path.join(root, "config")
  const data = path.join(root, "data")
  const state = path.join(root, "state")
  const serviceState = path.join(state, "opencode")
  const serviceConfig = path.join(config, "opencode")
  await Promise.all(
    [home, cache, config, data, state, serviceState, serviceConfig].map((directory) =>
      fs.mkdir(directory, { recursive: true }),
    ),
  )
  await fs.writeFile(path.join(serviceConfig, "service-local.json"), JSON.stringify({ port: await freePort() }))
  return {
    root,
    state: serviceState,
    env: {
      PATH: process.env.PATH ?? "",
      BUN_RUNTIME_TRANSPILER_CACHE_PATH: path.join(os.tmpdir(), "opencode-test-transpiler-cache"),
      HOME: home,
      USERPROFILE: home,
      OPENCODE_TEST_HOME: home,
      XDG_CACHE_HOME: cache,
      XDG_CONFIG_HOME: config,
      XDG_DATA_HOME: data,
      XDG_STATE_HOME: state,
      OPENCODE_DISABLE_MODELS_FETCH: "1",
    },
    async [Symbol.asyncDispose]() {
      await fs.rm(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 })
    },
  }
}

async function freePort() {
  const probe = net.createServer()
  await new Promise<void>((resolve, reject) => {
    probe.once("error", reject)
    probe.listen(0, "127.0.0.1", resolve)
  })
  const address = probe.address()
  await new Promise<void>((resolve, reject) => probe.close((error) => (error ? reject(error) : resolve())))
  if (address === null || typeof address === "string") throw new Error("the port probe reported no address")
  return address.port
}

export async function startStdio(command: readonly string[], env: Record<string, string>) {
  const subprocess = Bun.spawn([...command, "serve", "--stdio", "--port", "0"], {
    cwd: cliDirectory,
    env,
    stdin: "pipe",
    stdout: "pipe",
    stderr: "pipe",
  })
  const line = await Promise.race([
    readLine(subprocess.stdout),
    Bun.sleep(60_000).then(() => {
      subprocess.kill("SIGKILL")
      return undefined
    }),
  ])
  if (line === undefined) {
    subprocess.kill("SIGKILL")
    const stderr = await new Response(subprocess.stderr).text()
    throw new Error(`Server did not print its URL${stderr ? `: ${stderr}` : ""}`)
  }
  return {
    url: (JSON.parse(line) as { url: string }).url,
    process: subprocess,
    async [Symbol.asyncDispose]() {
      subprocess.kill("SIGTERM")
      await subprocess.exited
    },
  }
}

export async function runCommand(command: readonly string[], args: readonly string[], env: Record<string, string>) {
  const subprocess = Bun.spawn([...command, ...args], {
    cwd: cliDirectory,
    env,
    stdin: "ignore",
    stdout: "pipe",
    stderr: "pipe",
  })
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(subprocess.stdout).text(),
    new Response(subprocess.stderr).text(),
    subprocess.exited,
  ])
  return { stdout, stderr, exitCode }
}

async function readLine(stream: ReadableStream<Uint8Array>) {
  const decoder = new TextDecoder()
  let buffered = ""
  for await (const chunk of stream) {
    buffered += decoder.decode(chunk, { stream: true })
    const newline = buffered.indexOf("\n")
    if (newline !== -1) return buffered.slice(0, newline)
  }
  return buffered.trim() === "" ? undefined : buffered.trim()
}
