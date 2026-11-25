import { NodeFileSystem } from "@effect/platform-node"
import { Service } from "@opencode/client/effect/service"
import { ServerInfo } from "@opencode/protocol/groups/server"
import { expect, test } from "bun:test"
import { Effect, Schema } from "effect"
import fs from "node:fs/promises"
import path from "node:path"
import { rustBinary, sandbox } from "./support"

test("service contenders elect one owner and stop cleanly", async () => {
  await using box = await sandbox()
  const pluginRoot = box.env.HOME
  if (typeof pluginRoot !== "string" || pluginRoot.length === 0) throw new Error("Sandbox has no HOME directory")
  await fs.mkdir(path.join(pluginRoot, ".opencode", "plugins"), { recursive: true })
  const processes: Array<ReturnType<typeof Bun.spawn>> = []
  const stderr: Array<Promise<string>> = []
  let registration: string | undefined
  let failure: unknown
  try {
    const spawnService = () => {
      const subprocess = Bun.spawn([rustBinary(), "serve", "--service"], {
        env: box.env,
        stdout: "ignore",
        stderr: "pipe",
      })
      processes.push(subprocess)
      stderr.push(new Response(subprocess.stderr).text())
      return subprocess
    }
    spawnService()
    spawnService()

    registration = await waitForRegistration(box.state)
    const file = registration
    const info = await Schema.decodeUnknownPromise(Service.Info)(JSON.parse(await fs.readFile(file, "utf8")))
    expect(info.id).toBeDefined()
    expect(info.password).toBeDefined()
    if (info.id === undefined || info.password === undefined)
      throw new Error("Registration is missing service identity")

    const credential = btoa(`opencode:${info.password}`)
    const headers = { authorization: "Basic " + credential }
    const token = encodeURIComponent(credential)

    const serverInfo = await waitForReady(info.url, headers)
    expect(serverInfo.pid).toBe(info.pid)

    const tokenInfo = await fetch(new URL(`/api/info?auth_token=${token}`, info.url), {
      signal: AbortSignal.timeout(5_000),
    })
    expect(tokenInfo.status).toBe(200)
    const tokenOpenApi = await fetch(new URL(`/openapi.json?auth_token=${token}`, info.url), {
      signal: AbortSignal.timeout(5_000),
    })
    expect(tokenOpenApi.status).toBe(200)

    const plugin = path.join(pluginRoot, ".opencode", "plugins", "smoke.ts")
    if ((await pluginIDs(info.url, headers, pluginRoot)).includes("smoke"))
      throw new Error("Smoke plugin existed before creation")
    await fs.writeFile(plugin, pluginSource())
    await waitForPlugin(info.url, headers, pluginRoot, plugin)

    const unauthorizedInfo = await fetch(new URL("/api/info", info.url), {
      signal: AbortSignal.timeout(5_000),
    })
    expect(unauthorizedInfo.status).toBe(401)
    const unauthorizedOpenApi = await fetch(new URL("/openapi.json", info.url), {
      signal: AbortSignal.timeout(5_000),
    })
    expect(unauthorizedOpenApi.status).toBe(401)

    const stopRoute = await fetch(new URL("/api/service/stop", info.url), {
      method: "POST",
      headers: { ...headers, "content-type": "application/json" },
      body: JSON.stringify({ instanceID: info.id }),
      signal: AbortSignal.timeout(5_000),
    })
    expect(stopRoute.status).toBe(404)

    const winner = processes.find((subprocess) => subprocess.pid === info.pid)
    const loser = processes.find((subprocess) => subprocess.pid !== info.pid)
    expect(winner).toBeDefined()
    expect(loser).toBeDefined()
    if (winner === undefined || loser === undefined) throw new Error("Contenders did not elect one registered owner")
    expect(await exitedWithin(loser, 10_000)).toBe(true)

    await Effect.runPromise(Service.stop({ file }).pipe(Effect.provide(NodeFileSystem.layer)))
    expect(await exitedWithin(winner, 10_000)).toBe(true)
    expect(await removedWithin(file)).toBe(true)
  } catch (cause) {
    failure = cause
  } finally {
    for (const subprocess of processes) subprocess.kill("SIGKILL")
    await Promise.all(processes.map((subprocess) => subprocess.exited))
    if (registration !== undefined) await fs.rm(registration, { force: true })
  }
  if (failure !== undefined) {
    const output = await Promise.all(stderr)
    throw new Error(output.filter(Boolean).join("\n") || "Service lifecycle smoke test failed", { cause: failure })
  }
})

async function waitForRegistration(state: string) {
  for (let attempt = 0; attempt < 800; attempt++) {
    const files = await fs.readdir(state).catch(() => [] as string[])
    const file = files.find(
      (name) => name === "service.json" || (name.startsWith("service-") && name.endsWith(".json")),
    )
    if (file !== undefined) return path.join(state, file)
    await Bun.sleep(25)
  }
  throw new Error("Service did not publish registration")
}

async function waitForReady(url: string, headers: Record<string, string>) {
  const deadline = Date.now() + 20_000
  while (Date.now() < deadline) {
    const response = await fetch(new URL("/api/info", url), {
      headers,
      signal: AbortSignal.timeout(5_000),
    }).catch(() => undefined)
    if (response?.status === 200) return Schema.decodeUnknownPromise(ServerInfo)(await response.json())
    await Bun.sleep(25)
  }
  throw new Error("Service did not become ready")
}

function exitedWithin(subprocess: ReturnType<typeof Bun.spawn>, milliseconds: number) {
  return Promise.race([subprocess.exited.then(() => true), Bun.sleep(milliseconds).then(() => false)])
}

async function removedWithin(file: string) {
  for (let attempt = 0; attempt < 200; attempt++) {
    if (!(await Bun.file(file).exists())) return true
    await Bun.sleep(25)
  }
  return false
}

function pluginSource() {
  return 'export default { id: "smoke", setup: async () => {} }\n'
}

async function pluginIDs(url: string, headers: HeadersInit, directory: string) {
  const endpoint = new URL("/api/plugin", url)
  endpoint.searchParams.set("location[directory]", directory)
  const response = await fetch(endpoint, { headers, signal: AbortSignal.timeout(5_000) })
  if (response.status !== 200) throw new Error(`Plugin list returned ${response.status}`)
  const body: unknown = await response.json()
  if (typeof body !== "object" || body === null || !("data" in body) || !Array.isArray(body.data)) {
    throw new Error("Service returned an invalid plugin list")
  }
  return body.data.flatMap((plugin) =>
    typeof plugin === "object" && plugin !== null && "id" in plugin && typeof plugin.id === "string" ? [plugin.id] : [],
  )
}

async function waitForPlugin(url: string, headers: HeadersInit, directory: string, plugin: string) {
  const deadline = Date.now() + 10_000
  let attempt = 0
  while (Date.now() < deadline) {
    if ((await pluginIDs(url, headers, directory)).includes("smoke")) return
    await Bun.sleep(25)
    if (++attempt % 10 === 0) await fs.writeFile(plugin, `${pluginSource()}// watcher retry ${attempt}\n`)
  }
  throw new Error("Service did not discover the created plugin")
}
