import { Duration, Effect, Layer, LayerMap, Stream } from "effect"
import { App } from "@opencode/core/app"
import { AppNodeBuilder } from "@opencode/core/effect/app-node-builder"
import { ConfigPluginSource } from "@opencode/core/config/plugin/source"
import { Flag } from "@opencode/core/flag"
import { Instance } from "@opencode/core/instance"
import { Location } from "@opencode/core/location"
import { LocationServiceMap } from "@opencode/core/location-services"
import { Plugin } from "@opencode/core/plugin"
import { SdkPlugins } from "@opencode/core/plugin/sdk"
import { AbsolutePath } from "@opencode/core/schema"
import { Tool } from "@opencode/core/tool"
import { definition } from "@opencode/core/tool/runtime"
import { LayerNode } from "@opencode/util/effect/layer-node"
import { Global } from "@opencode/util/global"
import { Npm } from "@opencode/util/npm"
import { Bus } from "../src/bus"
import { Database } from "../src/database/database"
import { tempGlobalLayer } from "./fixture/global"
import { offlineModels } from "./fixture/models"
import { tmpdirScoped } from "./fixture/tmpdir"

const unexpected = (pkg: string) =>
  Effect.fail(new Npm.InstallFailedError({ add: [pkg], dir: "", cause: new Error(`Unexpected Npm call: ${pkg}`) }))

const instances = Layer.effect(
  LocationServiceMap.Service,
  Effect.gen(function* () {
    const map = yield* LayerMap.make(
      (ref: Location.Ref) => Instance.layer(ref, { discovery: false, replacements: bindings }),
      { idleTimeToLive: Duration.infinity },
    )
    const bindings: LayerNode.Replacements = [
      App.node.replace(App.configured({ channel: "dev" })),
      Global.node.replace(tempGlobalLayer),
      offlineModels,
      ConfigPluginSource.node.replace(
        Layer.succeed(
          ConfigPluginSource.Service,
          ConfigPluginSource.Service.of({ operations: () => Effect.succeed([]), changes: () => Stream.never }),
        ),
      ),
      Npm.node.replace(
        Layer.succeed(
          Npm.Service,
          Npm.Service.of({
            add: unexpected,
            resolve: (name) => Effect.succeed({ directory: "", name }),
            check: () => Effect.succeed(false),
            update: unexpected,
            follows: () => Effect.succeed(false),
            reject: () => Effect.succeed(false),
            which: () => Effect.undefined,
          }),
        ),
      ),
      LocationServiceMap.node.replace(Layer.succeed(LocationServiceMap.Service, map)),
      Instance.node.replace(
        Layer.succeed(Instance.Service, { provide: (session) => Effect.provide(map.get(session.location)) }),
      ),
    ]
    return map
  }),
)

const layer = AppNodeBuilder.build(LayerNode.group([Database.node, Bus.node, SdkPlugins.node, LocationServiceMap.node]), [
  Global.node.replace(tempGlobalLayer),
  offlineModels,
  LocationServiceMap.node.replace(instances),
])

const names = new Set(["read", "write", "edit", "patch", "glob", "grep", "skill"])

function rustDefinition(value: ReturnType<typeof definition>) {
  const result: Record<string, unknown> = {
    name: value.name,
    description: value.description,
    inputSchema: value.inputSchema,
  }
  if (value.outputSchema !== undefined) result.outputSchema = value.outputSchema
  if (value.freeform !== undefined) result.freeform = value.freeform
  return result
}

const probe = Effect.gen(function* () {
  const directory = yield* tmpdirScoped()
  const locations = yield* LocationServiceMap.Service
  return yield* Effect.gen(function* () {
    const plugins = yield* Plugin.Service
    yield* plugins.awaitActivation
    const tools = yield* Tool.Service
    const registered = yield* tools.list()
    return registered
      .filter((tool) => names.has(tool.name) && tool.options?.namespace === undefined)
      .map((tool) => ({
        ...rustDefinition(definition(tool)),
        ...(tool.options?.permission === undefined ? {} : { permission: tool.options.permission }),
        codemode: tool.options?.codemode ?? true,
      }))
  }).pipe(
    Effect.scoped,
    Effect.provide(locations.get(Location.Ref.make({ directory: AbsolutePath.make(directory.path) }))),
  )
}).pipe(Effect.scoped, Effect.provide(layer), Effect.provideService(Flag.Overrides, process.argv[2]))

const value = await Effect.runPromise(probe)
console.log(`__TOOL_DEFINITIONS__${JSON.stringify(value)}`)
