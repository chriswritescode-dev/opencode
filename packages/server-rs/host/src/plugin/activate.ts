import { Cause, Effect, Exit, Scope } from "effect"
import type { Context } from "@opencode/plugin/effect/plugin"
import type { LocationRef } from "../protocol.gen"
import { makeContext, type PluginList } from "./context"
import { definition, forget, locationKey } from "./load"

export type ActivateParams = {
  handle: string
  options: Context["options"]
  location: LocationRef
  app: Context["app"]
}

const activations = new Map<string, { scope: Scope.Closeable; locationKey: string }>()

export async function activate(params: ActivateParams, list: PluginList, callLocation: LocationRef) {
  const target = definition(params.handle)
  const scope = await Effect.runPromise(Scope.make())
  const exit = await Effect.runPromiseExit(
    target
      .effect({
        ...makeContext({ app: params.app, location: params.location, options: params.options, list }),
        options: params.options,
      })
      .pipe(Effect.provideService(Scope.Scope, scope)),
  )
  if (Exit.isFailure(exit)) {
    await Effect.runPromise(Scope.close(scope, exit))
    const error = new Error(Cause.pretty(exit.cause))
    error.name = "PluginActivationFailed"
    throw error
  }
  const activation = crypto.randomUUID()
  activations.set(activation, { scope, locationKey: locationKey(callLocation) })
  return { activation }
}

export async function deactivate(activation: string, location: LocationRef) {
  const found = activations.get(activation)
  if (found === undefined || found.locationKey !== locationKey(location)) return
  activations.delete(activation)
  await Effect.runPromise(Scope.close(found.scope, Exit.void))
}

export async function close(location: LocationRef) {
  const key = locationKey(location)
  const scopes = [...activations.entries()].filter(([, entry]) => entry.locationKey === key).reverse()
  const failures: Cause.Cause<never>[] = []
  for (const [activation, entry] of scopes) {
    activations.delete(activation)
    const exit = await Effect.runPromiseExit(Scope.close(entry.scope, Exit.void))
    if (Exit.isFailure(exit)) failures.push(exit.cause)
  }
  forget(location)
  if (failures.length === 0) return
  const error = new Error(failures.map((cause) => Cause.pretty(cause)).join("\n"))
  error.name = "PluginCloseFailed"
  throw error
}
