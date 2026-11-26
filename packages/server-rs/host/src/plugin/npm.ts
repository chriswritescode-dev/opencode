import { Cause, Effect, Exit, ManagedRuntime } from "effect"
import { Global } from "@opencode/util/global"
import { Npm } from "@opencode/util/npm"
import { LayerNode } from "@opencode/util/effect/layer-node"

export type NpmPaths = {
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

type Runtime = ManagedRuntime.ManagedRuntime<Npm.Service, never>

const runtimes = new Map<string, Runtime>()

function runtimeFor(paths: NpmPaths) {
  const key = JSON.stringify(paths)
  const existing = runtimes.get(key)
  if (existing !== undefined) return existing
  const runtime = ManagedRuntime.make(
    LayerNode.compile(Npm.node, { replacements: [Global.node.replace(Global.layerWith(paths))] }),
  )
  runtimes.set(key, runtime)
  return runtime
}

async function run<A, E>(
  runtime: Runtime,
  effect: Effect.Effect<A, E, Npm.Service>,
  message?: (cause: Cause.Cause<E>) => string,
) {
  const exit = await runtime.runPromiseExit(effect)
  if (Exit.isSuccess(exit)) return exit.value
  const cause = exit.cause
  const error = Cause.squash(cause) as { _tag?: string; message?: string } | undefined
  const failure = new Error(message ? message(cause) : (error?.message ?? Cause.pretty(cause)))
  failure.name = error?._tag ?? "Error"
  throw failure
}

export function npm(paths: NpmPaths) {
  return {
    add: (pkg: string) => run(runtimeFor(paths), Effect.flatMap(Npm.Service, (service) => service.add(pkg))),
    resolve: (pkg: string) => run(runtimeFor(paths), Effect.flatMap(Npm.Service, (service) => service.resolve(pkg))),
    check: (pkg: string) => run(runtimeFor(paths), Effect.flatMap(Npm.Service, (service) => service.check(pkg))),
    follows: (pkg: string) => run(runtimeFor(paths), Effect.flatMap(Npm.Service, (service) => service.follows(pkg))),
    reject: (pkg: string, version: string) =>
      run(runtimeFor(paths), Effect.flatMap(Npm.Service, (service) => service.reject(pkg, version))),
    update: (pkg: string) =>
      run(runtimeFor(paths), Effect.flatMap(Npm.Service, (service) => service.update(pkg)), Cause.pretty),
  }
}
