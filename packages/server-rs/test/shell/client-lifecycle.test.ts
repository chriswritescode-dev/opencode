import { NodeFileSystem } from "@effect/platform-node"
import { ensure, stop } from "@opencode/client/service"
import { Service } from "@opencode/client/effect/service"
import { expect, test } from "bun:test"
import { Effect, FileSystem } from "effect"
import { existsSync } from "node:fs"
import { readFile, rm } from "node:fs/promises"
import path from "node:path"
import { rustBinary, sandbox } from "./support"

type Registration = {
  readonly url: string
  readonly pid: number
  readonly version?: string
}

function run<A, E>(effect: Effect.Effect<A, E, FileSystem.FileSystem>) {
  return Effect.runPromise(effect.pipe(Effect.provide(NodeFileSystem.layer)))
}

function serviceCommand() {
  return [rustBinary(), "serve", "--service"]
}

async function readRegistration(file: string): Promise<Registration | undefined> {
  const value: unknown = await readFile(file, "utf8")
    .then((text) => JSON.parse(text))
    .catch(() => undefined)
  if (typeof value !== "object" || value === null) return undefined
  if (!("url" in value) || typeof value.url !== "string") return undefined
  if (!("pid" in value) || typeof value.pid !== "number") return undefined
  return {
    url: value.url,
    pid: value.pid,
    version: "version" in value && typeof value.version === "string" ? value.version : undefined,
  }
}

function isRunning(pid: number) {
  try {
    process.kill(pid, 0)
    return true
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ESRCH") return false
    throw error
  }
}

async function cleanup(file: string) {
  for (let attempt = 0; attempt < 50; attempt++) {
    const info = await readRegistration(file)
    if (info !== undefined) kill(info.pid)
    await rm(file, { force: true })
    if (await staysAbsent(file)) return
  }
  throw new Error(`Service registration ${file} keeps reappearing`)
}

async function staysAbsent(file: string) {
  for (let check = 0; check < 10; check++) {
    await Bun.sleep(100)
    if (existsSync(file)) return false
  }
  return true
}

function kill(pid: number) {
  try {
    process.kill(pid, "SIGKILL")
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error
  }
}

test("the effect client starts the service", async () => {
  await using box = await sandbox()
  const registration = path.join(box.state, "service-local.json")
  try {
    const endpoint = await run(Service.ensure({ file: registration, command: serviceCommand(), env: box.env }))
    const info = await readRegistration(registration)
    expect(info).toBeDefined()
    if (info === undefined) throw new Error("registration missing")
    expect(endpoint.url).toBe(info.url)
    expect(info.pid).toBeGreaterThan(0)
    expect(Number.isInteger(info.pid)).toBe(true)
    expect(typeof info.version).toBe("string")
    expect(info.version?.length ?? 0).toBeGreaterThan(0)
  } finally {
    await cleanup(registration)
  }
})

test("a second effect ensure reuses the running service", async () => {
  await using box = await sandbox()
  const registration = path.join(box.state, "service-local.json")
  try {
    await run(Service.ensure({ file: registration, command: serviceCommand(), env: box.env }))
    const first = await readRegistration(registration)
    expect(first).toBeDefined()
    if (first === undefined) throw new Error("registration missing")

    await run(Service.ensure({ file: registration, command: serviceCommand(), env: box.env }))
    const second = await readRegistration(registration)
    expect(second).toBeDefined()
    if (second === undefined) throw new Error("registration missing")
    expect(second.pid).toBe(first.pid)
  } finally {
    await cleanup(registration)
  }
})

test("concurrent promise ensure calls converge on one service", async () => {
  await using box = await sandbox()
  const registration = path.join(box.state, "service-local.json")
  try {
    const options = { file: registration, command: serviceCommand(), env: box.env }
    const [first, second] = await Promise.all([ensure(options), ensure(options)])
    expect(first.url).toBe(second.url)

    const info = await readRegistration(registration)
    expect(info).toBeDefined()
    if (info === undefined) throw new Error("registration missing")
    expect(info.pid).toBeGreaterThan(0)
    expect(Number.isInteger(info.pid)).toBe(true)
    expect(first.url).toBe(info.url)
  } finally {
    await cleanup(registration)
  }
})

test("the effect client discovers the running service", async () => {
  await using box = await sandbox()
  const registration = path.join(box.state, "service-local.json")
  try {
    await run(Service.ensure({ file: registration, command: serviceCommand(), env: box.env }))
    const info = await readRegistration(registration)
    expect(info).toBeDefined()
    if (info === undefined) throw new Error("registration missing")

    const endpoint = await run(Service.discover({ file: registration }))
    expect(endpoint).toBeDefined()
    if (endpoint === undefined) throw new Error("service not discovered")
    expect(endpoint.url).toBe(info.url)
  } finally {
    await cleanup(registration)
  }
})

test("the effect client stops the service and removes its registration", async () => {
  await using box = await sandbox()
  const registration = path.join(box.state, "service-local.json")
  try {
    await run(Service.ensure({ file: registration, command: serviceCommand(), env: box.env }))
    const info = await readRegistration(registration)
    expect(info).toBeDefined()
    if (info === undefined) throw new Error("registration missing")

    await run(Service.stop({ file: registration }))

    expect(existsSync(registration)).toBe(false)
    expect(isRunning(info.pid)).toBe(false)
  } finally {
    await cleanup(registration)
  }
})

test("discovery returns undefined after the service stops", async () => {
  await using box = await sandbox()
  const registration = path.join(box.state, "service-local.json")
  try {
    await run(Service.ensure({ file: registration, command: serviceCommand(), env: box.env }))
    await stop({ file: registration })

    const endpoint = await run(Service.discover({ file: registration }))
    expect(endpoint).toBeUndefined()
  } finally {
    await cleanup(registration)
  }
})

test("ensure replaces a registration whose process was killed", async () => {
  await using box = await sandbox()
  const registration = path.join(box.state, "service-local.json")
  try {
    await run(Service.ensure({ file: registration, command: serviceCommand(), env: box.env }))
    const killed = await readRegistration(registration)
    expect(killed).toBeDefined()
    if (killed === undefined) throw new Error("registration missing")

    process.kill(killed.pid, "SIGKILL")

    await run(Service.ensure({ file: registration, command: serviceCommand(), env: box.env }))
    const replacement = await readRegistration(registration)
    expect(replacement).toBeDefined()
    if (replacement === undefined) throw new Error("registration missing")
    expect(replacement.pid).not.toBe(killed.pid)
    expect(replacement.pid).toBeGreaterThan(0)

    const endpoint = await run(Service.discover({ file: registration }))
    expect(endpoint).toBeDefined()
    if (endpoint === undefined) throw new Error("service not discovered")
    expect(endpoint.url).toBe(replacement.url)
  } finally {
    await cleanup(registration)
  }
})
