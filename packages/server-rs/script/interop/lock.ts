import { mkdir, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { pathToFileURL } from "node:url"
import { upstreamCoreTree } from "../upstream"

type Kind = "flock" | "effect-flock"

type Message = {
  kind: Kind
  tree: string
  dir: string
  key: string
  action: "hold" | "try"
  timeoutMs: number
}

type Context = {
  binary: string
  tree: string
  dir: string
  expect: (name: string, expected: string, actual: string) => void
  fail: (name: string, detail: string) => never
}

const packageRoot = new URL("../..", import.meta.url).pathname
const file = new URL(import.meta.url).pathname
const key = "credential-refresh:cred_interop"

if (process.argv[2] === "worker") await worker(JSON.parse(process.argv[3] ?? ""))

export async function runLockInterop(input: { binary: string; directory: string; fail: (message: string) => never }) {
  const tree = await upstreamCoreTree()
  const dir = join(input.directory, "locks")
  await rm(dir, { recursive: true, force: true })
  await mkdir(dir, { recursive: true })
  const context: Context = {
    binary: input.binary,
    tree,
    dir,
    expect: (name, expected, actual) => {
      if (expected !== actual) input.fail(`interop: ${name}: expected ${expected}, got ${actual}`)
    },
    fail: (name, detail) => input.fail(`interop: ${name}: ${detail}`),
  }
  await typescriptHolds(context, "flock")
  await typescriptHolds(context, "effect-flock")
  await rustHolds(context)
  await released(context)
}

async function typescriptHolds(context: Context, kind: Kind) {
  const name = `lock: typescript ${kind} blocks rust`
  const holder = await hold(context, kind)
  try {
    context.expect(name, "timeout", rustTry(context, 1500))
  } finally {
    await stop(holder)
  }
  console.log(`interop: ${name}`)
}

async function rustHolds(context: Context) {
  const name = "lock: rust blocks both typescript flocks"
  const holder = Bun.spawn([context.binary, "hold", context.dir, key], {
    cwd: packageRoot,
    stdin: "pipe",
    stdout: "pipe",
    stderr: "pipe",
  })
  const line = await readLine(holder.stdout)
  if (line !== "held") {
    const stderr = await new Response(holder.stderr).text()
    holder.kill()
    context.fail(name, `the rust holder printed ${JSON.stringify(line)}: ${stderr.trim()}`)
  }
  try {
    context.expect(name, "timeout", await typescriptTry(context, "flock", 1500))
    context.expect(name, "timeout", await typescriptTry(context, "effect-flock", 1500))
  } finally {
    await stop(holder)
  }
  console.log(`interop: ${name}`)
}

async function released(context: Context) {
  const name = "lock: each side acquires the released lock"
  context.expect(name, "acquired", rustTry(context, 1500))
  context.expect(name, "acquired", await typescriptTry(context, "flock", 1500))
  context.expect(name, "acquired", await typescriptTry(context, "effect-flock", 1500))
  console.log(`interop: ${name}`)
}

async function hold(context: Context, kind: Kind) {
  const name = `lock: typescript ${kind} holds`
  const holder = spawnWorker(context, {
    kind,
    tree: context.tree,
    dir: context.dir,
    key,
    action: "hold",
    timeoutMs: 1500,
  })
  const line = await readLine(holder.stdout)
  if (line !== "held") {
    const stderr = await new Response(holder.stderr).text()
    holder.kill()
    context.fail(name, `the typescript holder printed ${JSON.stringify(line)}: ${stderr.trim()}`)
  }
  return holder
}

async function typescriptTry(context: Context, kind: Kind, timeoutMs: number) {
  const name = `lock: typescript ${kind} tries`
  const attempt = spawnWorker(context, {
    kind,
    tree: context.tree,
    dir: context.dir,
    key,
    action: "try",
    timeoutMs,
  })
  const line = await readLine(attempt.stdout)
  const code = await attempt.exited
  const stderr = await new Response(attempt.stderr).text()
  if (code !== 0 || line === undefined) context.fail(name, `the attempt exited ${code}: ${stderr.trim()}`)
  return line
}

function rustTry(context: Context, timeoutMs: number) {
  const name = "lock: rust tries"
  const result = Bun.spawnSync([context.binary, "try", context.dir, key, String(timeoutMs)], { cwd: packageRoot })
  if (result.exitCode !== 0)
    context.fail(name, `the rust attempt exited ${result.exitCode}: ${result.stderr.toString().trim()}`)
  return result.stdout.toString().trim()
}

function spawnWorker(context: Context, message: Message) {
  return Bun.spawn(["bun", "run", file, "worker", JSON.stringify(message)], {
    cwd: packageRoot,
    stdin: "pipe",
    stdout: "pipe",
    stderr: "pipe",
  })
}

async function stop(holder: ReturnType<typeof spawnWorker>) {
  holder.stdin?.end()
  await holder.exited
}

async function readLine(stream: ReadableStream<Uint8Array>) {
  const decoder = new TextDecoder()
  let buffered = ""
  for await (const chunk of stream) {
    buffered += decoder.decode(chunk, { stream: true })
    const newline = buffered.indexOf("\n")
    if (newline !== -1) return buffered.slice(0, newline)
  }
  const line = buffered.trim()
  return line === "" ? undefined : line
}

async function worker(message: Message) {
  if (message.kind === "flock") return workerFlock(message)
  return workerEffectFlock(message)
}

async function workerFlock(message: Message) {
  const { Flock } = await import(pathToFileURL(`${message.tree}/packages/util/src/flock.ts`).href)
  await run(message, (body) => Flock.withLock(message.key, body, { dir: message.dir, timeoutMs: message.timeoutMs }))
}

async function workerEffectFlock(message: Message) {
  const effect = await import(pathToFileURL(Bun.resolveSync("effect", `${message.tree}/packages/core/src`)).href)
  const { EffectFlock } = await import(pathToFileURL(`${message.tree}/packages/util/src/effect-flock.ts`).href)
  const { LayerNode } = await import(pathToFileURL(`${message.tree}/packages/util/src/effect/layer-node.ts`).href)
  const { Global } = await import(pathToFileURL(`${message.tree}/packages/util/src/global.ts`).href)
  const testGlobal = Global.layerWith({
    home: process.env.HOME ?? "",
    data: tmpdir(),
    cache: tmpdir(),
    config: tmpdir(),
    state: tmpdir(),
    bin: tmpdir(),
    log: tmpdir(),
    repos: tmpdir(),
    tmp: tmpdir(),
  })
  const layer = LayerNode.compile(EffectFlock.node, { replacements: [Global.node.replace(testGlobal)] })
  await run(message, (body) =>
    effect.Effect.runPromise(
      effect.Effect.scoped(
        effect.Effect.gen(function* () {
          const flock = yield* EffectFlock.Service
          yield* flock.acquire(message.key, message.dir, { timeoutMs: message.timeoutMs })
          yield* effect.Effect.promise(body)
        }),
      ).pipe(effect.Effect.provide(layer)),
    ),
  )
}

async function run(message: Message, withLock: (body: () => Promise<void>) => Promise<void>) {
  try {
    await withLock(async () => {
      console.log(message.action === "hold" ? "held" : "acquired")
      if (message.action === "hold") await closed()
    })
  } catch (error) {
    if (message.action === "try" && isTimeout(error)) {
      console.log("timeout")
      return
    }
    throw error
  }
}

function isTimeout(error: unknown) {
  if (typeof error !== "object" || error === null) return false
  if ("_tag" in error && error._tag === "LockTimeoutError") return true
  return (
    "message" in error && typeof error.message === "string" && error.message.startsWith("Timed out waiting for lock")
  )
}

function closed() {
  return new Promise<void>((resolve) => {
    process.stdin.once("end", resolve)
    process.stdin.once("close", resolve)
    process.stdin.resume()
  })
}
