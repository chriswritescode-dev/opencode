import { mkdir } from "node:fs/promises"
import { upstreamCoreTree } from "./upstream"

const root = new URL("..", import.meta.url).pathname
const ENGINE_TIMEOUT_MS = 30 * 60 * 1000

export const VARIANTS = [
  "user",
  "empty",
  "split-index",
  "skip-hash",
  "autocrlf-and-no-untracked-cache",
  "fsmonitor",
  "template-hook",
] as const

const ANCHOR = String.raw`function snapshotLayer(data: string, directory: string) {
  return AppNodeBuilder.build(Snapshot.node, [
    Location.node.replace(Location.boundNode(Location.Ref.make({ directory: AbsolutePath.make(directory) }))),
    Global.node.replace(Global.layerWith({ data, config: path.join(data, "config") })),
  ])
}`

const ADAPTED = String.raw`function snapshotLayer(data: string, directory: string) {
  if (process.env.PARITY_ENGINE !== "rust")
    return AppNodeBuilder.build(Snapshot.node, [
      Location.node.replace(Location.boundNode(Location.Ref.make({ directory: AbsolutePath.make(directory) }))),
      Global.node.replace(Global.layerWith({ data, config: path.join(data, "config") })),
    ])
  const driver = ((globalThis as any).__paritySnapshotDriver ??= startParitySnapshotDriver())
  const call = (request: unknown) =>
    Effect.tryPromise({
      try: () => driver(request),
      catch: () => new Snapshot.Error({ operation: "capture", message: "snapshot driver request failed" }),
    })
  const proxy = {
    capture: () =>
      call({ op: "capture", data, directory }).pipe(
        Effect.map((tree) => (typeof tree === "string" ? (tree as Snapshot.ID) : undefined)),
        Effect.catch(() => Effect.succeed(undefined)),
      ),
    files: (input: { from: Snapshot.ID; to: Snapshot.ID }) =>
      call({ op: "files", data, directory, from: input.from, to: input.to }).pipe(
        Effect.map((paths) => (paths as string[]).map((file) => RelativePath.make(file))),
      ),
    diff: (input: { from: Snapshot.ID; to: Snapshot.ID; paths?: readonly RelativePath[] }) =>
      call({
        op: "diff",
        data,
        directory,
        from: input.from,
        to: input.to,
        paths: input.paths ? [...input.paths] : undefined,
      }),
    restore: (input: { files: ReadonlyMap<RelativePath, Snapshot.ID> }) =>
      call({ op: "restore", data, directory, files: [...input.files] }).pipe(Effect.asVoid),
    transform: () => Effect.succeed({ dispose: Effect.void }),
    reload: () => Effect.void,
  }
  return Layer.succeed(Snapshot.Service, Snapshot.Service.of(proxy as any))

  function startParitySnapshotDriver() {
    const child = Bun.spawn([process.env.PARITY_SNAPSHOT_DRIVER!], {
      stdin: "pipe",
      stdout: "pipe",
      stderr: "inherit",
      env: process.env,
    })
    child.unref()
    const reader = child.stdout.getReader()
    const decoder = new TextDecoder()
    let buffered = ""
    process.once("beforeExit", () => {
      child.stdin.write(JSON.stringify({ op: "close" }) + "\n")
      child.stdin.end()
      reader.cancel().catch(() => undefined)
    })
    return async (request: unknown) => {
      child.stdin.write(JSON.stringify(request) + "\n")
      await child.stdin.flush()
      while (buffered.indexOf("\n") === -1) {
        const chunk = await reader.read()
        if (chunk.done) throw new Error("snapshot driver exited")
        buffered += decoder.decode(chunk.value, { stream: true })
      }
      const newline = buffered.indexOf("\n")
      const line = buffered.slice(0, newline)
      buffered = buffered.slice(newline + 1)
      const response = JSON.parse(line)
      if (response.error) throw new Error("snapshot driver request failed")
      return response.ok
    }
  }
}`

export function compareSnapshotOutputs(left: Record<string, unknown[]>, right: Record<string, unknown[]>) {
  const names = [...Object.keys(left), ...Object.keys(right).filter((name) => !(name in left))]
  const same: string[] = []
  const different: { name: string; steps: { index: number; left: unknown; right: unknown }[] }[] = []
  for (const name of names) {
    const a = left[name] ?? []
    const b = right[name] ?? []
    const steps = Array.from({ length: Math.max(a.length, b.length) }, (_, index) => index)
      .filter((index) => JSON.stringify(a[index]) !== JSON.stringify(b[index]))
      .map((index) => ({ index, left: a[index], right: b[index] }))
    if (steps.length === 0) same.push(name)
    else different.push({ name, steps })
  }
  return { same, different, identical: different.length === 0 }
}

export function formatSnapshotComparison(result: ReturnType<typeof compareSnapshotOutputs>) {
  const lines = result.same.map((name) => `same      ${name}`)
  for (const scenario of result.different) {
    lines.push(`DIFFERENT ${scenario.name}`)
    for (const step of scenario.steps) {
      lines.push(`  step ${step.index}`)
      lines.push(`    ts:   ${JSON.stringify(step.left)}`)
      lines.push(`    rust: ${JSON.stringify(step.right)}`)
    }
  }
  lines.push(result.identical ? "all scenarios identical" : `${result.different.length} divergent scenarios`)
  return lines
}

export function snapshotComparisonStatus(result: ReturnType<typeof compareSnapshotOutputs>) {
  return result.identical ? 0 : 1
}

if (import.meta.main) {
  try {
    await main()
  } catch (error) {
    console.error(`snapshot-parity: ${error instanceof Error ? error.message : String(error)}`)
    process.exit(2)
  }
}

async function main() {
  const argv = process.argv.slice(2)
  const keep = argv.includes("--keep")
  const selected = argv.filter((argument) => argument !== "--keep")
  for (const variant of selected)
    if (!(VARIANTS as readonly string[]).includes(variant)) fail(`unknown variant ${variant}`)

  const tree = await upstreamCoreTree()
  const driver = await buildDriver()
  await writeEngine(tree)
  const outputRoot = `${root}target/snapshot-parity`
  await mkdir(outputRoot, { recursive: true })

  let status = 0
  for (const variant of selected.length ? selected : VARIANTS) {
    console.log(`running ${variant}`)
    const workRoot = `${outputRoot}/work/${variant}`
    const tsRoot = `${workRoot}/ts`
    const rustRoot = `${workRoot}/rust`
    await mkdir(tsRoot, { recursive: true })
    await mkdir(rustRoot, { recursive: true })
    const tsOutput = `${outputRoot}/${variant}-ts.json`
    const rustOutput = `${outputRoot}/${variant}-rust.json`
    const config = variant.replaceAll("-", " ")
    const shared = {
      PARITY_CONFIG: config,
      PARITY_KEEP: keep ? "1" : undefined,
    }
    await runEngine(tree, ["bun", "run", "script/snapshot-parity.ts", tsOutput], {
      ...process.env,
      ...shared,
      PARITY_ENGINE: "ts",
      SNAPSHOT_BENCH_ROOT: tsRoot,
    })
    await runEngine(tree, ["bun", "run", "script/snapshot-parity-engine.ts", rustOutput], {
      ...process.env,
      ...shared,
      PARITY_ENGINE: "rust",
      PARITY_SNAPSHOT_DRIVER: driver,
      SNAPSHOT_BENCH_ROOT: rustRoot,
    })
    const ts: Record<string, unknown[]> = await Bun.file(tsOutput).json()
    const rust: Record<string, unknown[]> = await Bun.file(rustOutput).json()
    const result = compareSnapshotOutputs(ts, rust)
    for (const line of formatSnapshotComparison(result)) console.log(line)
    status = Math.max(status, snapshotComparisonStatus(result))
  }
  process.exitCode = status
}

async function buildDriver() {
  const proc = Bun.spawn(
    [
      "cargo",
      "build",
      "--locked",
      "-p",
      "opencode-vcs",
      "--example",
      "snapshot_driver",
      "--message-format=json-render-diagnostics",
    ],
    { cwd: root, stdout: "pipe", stderr: "inherit" },
  )
  const stdout = await new Response(proc.stdout).text()
  if ((await proc.exited) !== 0) fail("cargo build failed")
  for (const line of stdout.split("\n")) {
    if (!line.startsWith("{")) continue
    const message: { reason?: string; target?: { name?: string }; executable?: string } = JSON.parse(line)
    if (message.reason === "compiler-artifact" && message.target?.name === "snapshot_driver" && message.executable)
      return message.executable
  }
  fail("cargo build produced no snapshot_driver executable")
}

async function writeEngine(tree: string) {
  const source = `${tree}/packages/core/script/snapshot-parity.ts`
  const text = await Bun.file(source).text()
  const occurrences = text.split(ANCHOR).length - 1
  if (occurrences !== 1) fail(`expected one snapshotLayer in ${source}, found ${occurrences}`)
  await Bun.write(`${tree}/packages/core/script/snapshot-parity-engine.ts`, text.replace(ANCHOR, ADAPTED))
}

async function runEngine(tree: string, command: string[], env: Record<string, string | undefined>) {
  const proc = Bun.spawn(command, { cwd: `${tree}/packages/core`, env, stdout: "pipe", stderr: "pipe" })
  const timeout = setTimeout(() => proc.kill(), ENGINE_TIMEOUT_MS)
  const [stdout, stderr, code] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ])
  clearTimeout(timeout)
  if (code === 0) return
  if (stdout.length) process.stderr.write(stdout)
  if (stderr.length) process.stderr.write(stderr)
  fail(`${command.join(" ")} exited ${code}`)
}

function fail(message: string): never {
  console.error(`snapshot-parity: ${message}`)
  process.exit(2)
}
