import { Cause, Duration, Effect, Exit, Layer, LayerMap, Stream } from "effect"
import { Agent } from "@opencode/core/agent"
import { App } from "@opencode/core/app"
import { AppNodeBuilder } from "@opencode/core/effect/app-node-builder"
import { ConfigPluginSource } from "@opencode/core/config/plugin/source"
import { Flag } from "@opencode/core/flag"
import { Instance } from "@opencode/core/instance"
import { Location } from "@opencode/core/location"
import { LocationServiceMap } from "@opencode/core/location-services"
import { Plugin } from "@opencode/core/plugin"
import { PluginHooks } from "@opencode/core/plugin/hooks"
import { SdkPlugins } from "@opencode/core/plugin/sdk"
import { AbsolutePath } from "@opencode/core/schema"
import { Session } from "@opencode/core/session"
import { SessionMessage } from "@opencode/core/session/message"
import { Tool } from "@opencode/core/tool"
import { modelTools } from "@opencode/core/tool/model-policy"
import { definition } from "@opencode/core/tool/runtime"
import type { Model } from "@opencode/schema/model"
import type { Info } from "@opencode/schema/tool"
import { LayerNode } from "@opencode/util/effect/layer-node"
import { Global } from "@opencode/util/global"
import { Npm } from "@opencode/util/npm"
import { Bus } from "../src/bus"
import { Database } from "../src/database/database"
import { tempGlobalLayer } from "./fixture/global"
import { offlineModels } from "./fixture/models"
import { tmpdirScoped } from "./fixture/tmpdir"
import { toolNodeReplacements } from "./lib/tool"

const area = process.argv[2]

const porter = new Set(["read", "write", "edit", "patch", "glob", "grep", "skill"])
const denied = [
  "execute",
  "shell",
  "question",
  "subagent",
  "webfetch",
  "websearch",
  "experimental_image_generate",
  "experimental_video_generate",
] as const

const textSchema = {
  type: "object",
  properties: { text: { type: "string" } },
  required: ["text"],
  additionalProperties: false,
}

interface Rule {
  action: string
  resource: string
  effect: "allow" | "deny" | "ask"
}

type Add = {
  name: string
  description?: string
  namespace?: string
  permission?: string
  codemode?: boolean
  input?: "text" | "missing"
  output?: "text" | "missing"
  text?: string
}

interface SnapshotCase {
  name: string
  namespaces?: { name: string; description?: string }[]
  adds?: Add[]
  removes?: string[]
  permissions?: Rule[]
  hidden?: string[]
  calls?: { name: string; input: unknown }[]
}

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

async function exportDefinitions() {
  const off = await definitionsFor("-full_codemode", "dev", false)
  const on = await definitionsFor("full_codemode", "dev", true)
  console.log(`__TOOL_CORPUS__${JSON.stringify([...off, ...on])}`)
}

async function definitionsFor(override: string, channel: string, fullCodemode: boolean) {
  const layer = appLayer(channel)
  return Effect.runPromise(
    Effect.gen(function* () {
      const directory = yield* tmpdirScoped()
      const locations = yield* LocationServiceMap.Service
      return yield* Effect.gen(function* () {
        const plugins = yield* Plugin.Service
        yield* plugins.awaitActivation
        const tools = yield* Tool.Service
        const tools_registered = yield* tools.list()
        const ported = tools_registered.filter((tool) => porter.has(tool.name) && tool.options?.namespace === undefined)
        const nonGpt = { id: "claude-sonnet-4", modelID: "claude-sonnet-4" } as unknown as Model.Info
        const gpt = { id: "gpt-5", modelID: "gpt-5" } as unknown as Model.Info
        const partitions = fullCodemode
          ? [{ name: "full-codemode", model: nonGpt, fullCodemode: true, editDenied: false }]
          : [
              { name: "default-non-gpt", model: nonGpt, fullCodemode: false, editDenied: false },
              { name: "gpt-5", model: gpt, fullCodemode: false, editDenied: false },
              { name: "edit-denied", model: nonGpt, fullCodemode: false, editDenied: true },
            ]
        const cases = []
        for (const partition of partitions) {
          const permissions: Rule[] = denied.map((action) => ({ action, resource: "*", effect: "deny" }))
          for (const action of ["browser*", "opencode_*", "automation_*", "machine_*"]) {
            permissions.push({ action, resource: "*", effect: "deny" })
          }
          if (partition.editDenied) permissions.push({ action: "edit", resource: "*", effect: "deny" })
          const policy = modelTools({
            model: partition.model,
            websearch: false,
            fullCodeMode: partition.fullCodemode,
          })
          const snapshot = yield* tools.snapshot(permissions, {
            tools: { hidden: new Set([...policy.hidden, "opencode_worktree_remove"]) },
          })
          const unported = snapshot.definitions.map((tool) => tool.name).filter((name) => !porter.has(name))
          if (unported.length > 0) {
            throw new Error(`corpus: ${partition.name} kept tools this task does not port: ${unported.join(", ")}`)
          }
          cases.push({
            name: partition.name,
            full_codemode: partition.fullCodemode,
            permissions,
            hidden: [...policy.hidden, "opencode_worktree_remove"].sort(),
            definitions: snapshot.definitions.map(rustDefinition),
          })
        }
        return cases
      }).pipe(
        Effect.scoped,
        Effect.provide(locations.get(Location.Ref.make({ directory: AbsolutePath.make(directory.path) }))),
      )
    }).pipe(Effect.scoped, Effect.provide(layer), Effect.provideService(Flag.Overrides, override)),
  )
}

function appLayer(channel: string) {
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
        App.node.replace(App.configured({ channel })),
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
  return AppNodeBuilder.build(LayerNode.group([Database.node, Bus.node, SdkPlugins.node, LocationServiceMap.node]), [
    Global.node.replace(tempGlobalLayer),
    offlineModels,
    LocationServiceMap.node.replace(instances),
  ])
}

async function exportSnapshot() {
  const registry = AppNodeBuilder.build(LayerNode.group([Tool.node, PluginHooks.node]), [...toolNodeReplacements()])
  const identity = {
    agent: Agent.ID.make("build"),
    messageID: SessionMessage.ID.make("msg_registry"),
  }
  const sessionID = Session.ID.make("ses_registry")
  const denyExecute: Rule = { action: "execute", resource: "*", effect: "deny" }

  const cases: SnapshotCase[] = [
    {
      name: "flatten-and-latest-wins",
      adds: [
        { name: "before", text: "before" },
        { name: "", text: "invalid" },
        { name: "echo.tool", text: "first" },
        { name: "echo_tool", text: "last" },
        { name: "execute", codemode: false, text: "reserved" },
        { name: "after", text: "after" },
      ],
      calls: [
        { name: "before", input: { text: "before" } },
        { name: "echo_tool", input: { text: "last" } },
        { name: "after", input: { text: "after" } },
      ],
    },
    {
      name: "nonletter-names",
      adds: [
        { name: "2d_get_scene", text: "2d_get_scene" },
        { name: "123", text: "123" },
        { name: "_lookup", text: "_lookup" },
        { name: "-lookup", text: "-lookup" },
        { name: "2d_get_scene", namespace: "123._private.-tools", text: "namespaced" },
      ],
      calls: [
        { name: "2d_get_scene", input: { text: "digit" } },
        { name: "123", input: { text: "numeric" } },
        { name: "_lookup", input: { text: "underscore" } },
        { name: "-lookup", input: { text: "hyphen" } },
        { name: "123__private_-tools_2d_get_scene", input: { text: "namespaced" } },
      ],
    },
    {
      name: "canonical-order",
      adds: [
        { name: "zeta", text: "zeta" },
        { name: "alpha", text: "alpha" },
        { name: "beta", namespace: "alpha", text: "beta" },
        { name: "echo", text: "echo" },
      ],
    },
    {
      name: "invalid-namespace",
      namespaces: [{ name: "invalid..namespace", description: "Invalid" }],
      adds: [
        { name: "first", text: "first" },
        { name: "second", namespace: "invalid..namespace", text: "second" },
        { name: "third", namespace: "invalid__namespace", text: "third" },
      ],
    },
    {
      name: "deny-question-alias",
      adds: [
        { name: "question", codemode: false, text: "question" },
        { name: "bash", codemode: false, text: "bash" },
        { name: "edit", permission: "edit", codemode: false, text: "edit" },
        { name: "write", permission: "edit", codemode: false, text: "write" },
      ],
      permissions: [{ action: "question", resource: "*", effect: "deny" }],
    },
    {
      name: "deny-all-then-question-private",
      adds: [
        { name: "question", codemode: false, text: "question" },
        { name: "bash", codemode: false, text: "bash" },
        { name: "edit", permission: "edit", codemode: false, text: "edit" },
        { name: "write", permission: "edit", codemode: false, text: "write" },
      ],
      permissions: [
        { action: "*", resource: "*", effect: "deny" },
        { action: "question", resource: "private", effect: "allow" },
      ],
    },
    {
      name: "question-private-then-deny-all",
      adds: [
        { name: "question", codemode: false, text: "question" },
        { name: "bash", codemode: false, text: "bash" },
        { name: "edit", permission: "edit", codemode: false, text: "edit" },
        { name: "write", permission: "edit", codemode: false, text: "write" },
      ],
      permissions: [
        { action: "question", resource: "private", effect: "allow" },
        { action: "*", resource: "*", effect: "deny" },
      ],
    },
    {
      name: "deny-edit-alias",
      adds: [
        { name: "question", codemode: false, text: "question" },
        { name: "bash", codemode: false, text: "bash" },
        { name: "edit", permission: "edit", codemode: false, text: "edit" },
        { name: "write", permission: "edit", codemode: false, text: "write" },
      ],
      permissions: [{ action: "edit", resource: "*", effect: "deny" }],
    },
    {
      name: "permission-isolation",
      adds: [
        { name: "first", codemode: false, text: "first" },
        { name: "second", permission: "edit", codemode: false, text: "second" },
      ],
      permissions: [{ action: "edit", resource: "*", effect: "deny" }],
    },
    {
      name: "missing-input",
      adds: [
        { name: "echo", input: "missing", output: "missing", text: "echo" },
        { name: "direct", codemode: false, input: "missing", text: "direct" },
      ],
      calls: [{ name: "direct", input: { text: "direct" } }],
    },
    {
      name: "removes-registration",
      adds: [
        { name: "keep", codemode: false, text: "keep" },
        { name: "drop", codemode: false, text: "drop" },
      ],
      removes: ["drop"],
      calls: [{ name: "drop", input: { text: "drop" } }],
    },
  ]

  const exported = []
  for (const item of cases) {
    const permissions = [denyExecute, ...(item.permissions ?? [])]
    const result = await Effect.runPromise(
      Effect.gen(function* () {
        const service = yield* Tool.Service
        yield* service.transform((editor) => {
          for (const namespace of item.namespaces ?? []) editor.namespace(namespace)
          for (const add of item.adds ?? []) editor.add(makeTool(add))
        })
        if (item.removes !== undefined)
          yield* service.transform((editor) => item.removes!.forEach((id) => editor.remove(id)))
        const snapshot = yield* service.snapshot(permissions, { tools: { hidden: new Set(item.hidden ?? []) } })
        const calls = []
        for (const call of item.calls ?? []) {
          const exit = yield* snapshot
            .execute({
              sessionID,
              ...identity,
              call: { type: "tool-call", id: `call-${call.name}`, name: call.name, input: call.input },
            })
            .pipe(Effect.exit)
          if (Exit.isSuccess(exit)) calls.push({ name: call.name, status: "completed", output: exit.value.output })
          else {
            const failure = Cause.squash(exit.cause)
            const message = failure instanceof Error ? failure.message : String(failure)
            calls.push({ name: call.name, status: "error", message })
          }
        }
        return {
          definitions: snapshot.definitions.map(rustDefinition),
          ids: (yield* service.list()).map((tool) => tool.id),
          calls,
        }
      }).pipe(Effect.scoped, Effect.provide(registry)),
    )
    exported.push({
      name: item.name,
      namespaces: item.namespaces ?? [],
      adds: item.adds ?? [],
      removes: item.removes ?? [],
      permissions,
      hidden: item.hidden ?? [],
      calls: item.calls ?? [],
      expected: result,
    })
  }
  console.log(`__TOOL_CORPUS__${JSON.stringify(exported)}`)
}

function makeTool(add: Add): Info {
  const value = add.text ?? add.name
  return {
    name: add.name,
    description: add.description ?? "Return text",
    input: add.input === "missing" ? undefined : textSchema,
    output: add.output === "missing" ? undefined : textSchema,
    options: {
      codemode: add.codemode ?? false,
      ...(add.permission === undefined ? {} : { permission: add.permission }),
      ...(add.namespace === undefined ? {} : { namespace: add.namespace }),
    },
    execute: () => Effect.succeed({ output: { text: value }, content: value }),
  } as unknown as Info
}

if (area === "definitions") await exportDefinitions()
else if (area === "snapshot") await exportSnapshot()
else {
  console.error(`corpus: unknown area ${area ?? ""}`)
  process.exit(1)
}
