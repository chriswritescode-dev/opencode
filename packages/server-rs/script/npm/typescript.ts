import { dirname, join } from "node:path"
import { pathToFileURL } from "node:url"
import { upstreamCoreTree } from "../upstream"

type NpmPaths = {
  cache: string
  state: string
  data: string
  config: string
  tmp: string
  bin: string
  log: string
  repos: string
  home: string
}

type Graph = { replace: (replacement: unknown) => unknown }

type NpmService = {
  add: (pkg: string) => unknown
  resolve: (pkg: string) => unknown
  update: (pkg: string) => unknown
  reject: (pkg: string, version: string) => unknown
}

type NpmModule = { Npm: { node: unknown; Service: unknown } }

type GlobalModule = {
  Global: { node: Graph; layerWith: (paths: NpmPaths) => unknown }
}

type LayerNodeModule = {
  LayerNode: { compile: (graph: unknown, options: { replacements: ReadonlyArray<unknown> }) => unknown }
}

const usage =
  "usage: bun run script/npm/typescript.ts <add|resolve|update|reject> <pkg> [version] --cache <dir> --state <dir>"
const actions = new Set(["add", "resolve", "update", "reject"])

function fail(message: string): never {
  process.stderr.write(`${message}\n`)
  process.exit(1)
}

function parseArgs(args: ReadonlyArray<string>) {
  const positional: Array<string> = []
  const options: { cache?: string; state?: string } = {}
  let index = 0
  while (index < args.length) {
    const arg = args[index]
    if (arg === "--cache" || arg === "--state") {
      const value = args[index + 1]
      if (value === undefined) fail(`${arg} requires a directory`)
      options[arg === "--cache" ? "cache" : "state"] = value
      index += 2
      continue
    }
    if (arg.startsWith("--")) fail(`unknown option ${arg}`)
    positional.push(arg)
    index += 1
  }
  return { positional, cache: options.cache, state: options.state }
}

function globalPaths(cache: string, state: string): NpmPaths {
  const base = dirname(cache) || cache
  return {
    cache,
    state,
    data: join(base, "data"),
    config: join(base, "config"),
    tmp: join(base, "tmp"),
    bin: join(base, "bin"),
    log: join(base, "log"),
    repos: join(base, "repos"),
    home: join(base, "home"),
  }
}

async function execute(action: string, pkg: string, version: string | undefined, paths: NpmPaths) {
  const tree = await upstreamCoreTree()
  const resolve = (name: string) => import(pathToFileURL(Bun.resolveSync(name, `${tree}/packages/core/src`)).href)
  const load = (path: string) => import(pathToFileURL(`${tree}/packages/${path}`).href)
  const effect = await resolve("effect")
  const global: GlobalModule = await load("util/src/global.ts")
  const npm: NpmModule = await load("util/src/npm.ts")
  const layerNode: LayerNodeModule = await load("util/src/effect/layer-node.ts")
  const runtime = effect.ManagedRuntime.make(
    layerNode.LayerNode.compile(npm.Npm.node, {
      replacements: [global.Global.node.replace(global.Global.layerWith(paths))],
    }),
  )
  const program = effect.Effect.flatMap(npm.Npm.Service, (service: NpmService) => {
    if (action === "add") return service.add(pkg)
    if (action === "resolve") return service.resolve(pkg)
    if (action === "update") return service.update(pkg)
    if (action !== "reject" || version === undefined) return fail(usage)
    return service.reject(pkg, version)
  })
  try {
    return await runtime.runPromise(program)
  } finally {
    await runtime.dispose()
  }
}

const { positional, cache, state } = parseArgs(process.argv.slice(2))
const action = positional.at(0)
const pkg = positional.at(1)
const version = positional.at(2)

if (action === undefined || pkg === undefined) fail(usage)
if (!actions.has(action)) fail(usage)
if (action === "reject" ? version === undefined : version !== undefined) fail(usage)
if (positional.length > (action === "reject" ? 3 : 2)) fail(usage)
if (cache === undefined || state === undefined) fail(usage)

try {
  const result = await execute(action, pkg, version, globalPaths(cache, state))
  process.stdout.write(`${JSON.stringify(result)}\n`)
} catch (error) {
  process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`)
  process.exit(1)
}
