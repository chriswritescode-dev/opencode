import { Cause, Duration, Effect, Exit, Layer, LayerMap, Result, Stream } from "effect"
import fs from "node:fs/promises"
import os from "node:os"
import path from "node:path"
import { Agent } from "@opencode/core/agent"
import { App } from "@opencode/core/app"
import { AppNodeBuilder } from "@opencode/core/effect/app-node-builder"
import { ConfigPluginSource } from "@opencode/core/config/plugin/source"
import { Environment } from "@opencode/core/environment/index"
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
import { EditTool } from "@opencode/core/tool/plugin/edit"
import { PatchTool } from "@opencode/core/tool/plugin/patch"
import { ReadTool } from "@opencode/core/tool/plugin/read"
import { WriteTool } from "@opencode/core/tool/plugin/write"
import { ReadToolFileSystem } from "@opencode/core/tool/read-filesystem"
import { definition, execute as executeTool } from "@opencode/core/tool/runtime"
import { ToolOutput } from "@opencode/core/tool-output"
import type { Model } from "@opencode/schema/model"
import type { Info } from "@opencode/schema/tool"
import { LayerNode } from "@opencode/util/effect/layer-node"
import { LayerNodePlatform } from "@opencode/util/effect/app-node-platform"
import { CrossSpawnSpawner } from "@opencode/util/cross-spawn-spawner"
import { FSUtil } from "@opencode/util/fs-util"
import { Global } from "@opencode/util/global"
import { Npm } from "@opencode/util/npm"
import { Patch } from "@opencode/util/patch"
import { ChildProcessSpawner } from "effect/process"
import { Bus } from "../src/bus"
import { Database } from "../src/database/database"
import { tempGlobalLayer } from "./fixture/global"
import { offlineModels } from "./fixture/models"
import { tmpdirScoped } from "./fixture/tmpdir"
import { Config } from "@opencode/core/config"
import { FileAccess } from "@opencode/core/file-access"
import { FileMutation } from "@opencode/core/file-mutation"
import { FileWrite } from "@opencode/core/file-write"
import { Formatter } from "@opencode/core/formatter"
import { Image } from "@opencode/core/image"
import { InstructionDiscovery } from "@opencode/core/instruction-discovery"
import { Permission } from "@opencode/core/permission"
import { Skill } from "@opencode/core/skill"
import { SessionInstructions } from "@opencode/core/session/instructions"
import { toSessionError } from "@opencode/core/session/to-session-error"
import { makeLocationNode } from "@opencode/util/effect/app-node"
import { hostEnvironmentLayer } from "./fixture/environment"
import { location } from "./fixture/location"
import { permissionLayer } from "./lib/permission"
import { registerToolPlugin, toolIdentity, toolNodeReplacements } from "./lib/tool"
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

interface ValidationCase {
  tool: string
  input: unknown
}

const validationCases: ValidationCase[] = [
  { tool: "read", input: {} },
  { tool: "read", input: { path: 1 } },
  { tool: "read", input: { path: "a", offset: "x" } },
  { tool: "read", input: { path: "a", offset: 1.5 } },
  { tool: "read", input: { path: "a", limit: -1 } },
  { tool: "read", input: { offset: "x", limit: "y", extra: true } },
  { tool: "glob", input: { pattern: "x", limit: 0 } },
  { tool: "glob", input: { pattern: "x", hidden: "yes" } },
  { tool: "glob", input: { pattern: 1 } },
  { tool: "glob", input: { pattern: "x", path: 1 } },
  { tool: "grep", input: { pattern: "" } },
  { tool: "grep", input: {} },
  { tool: "write", input: { path: "a" } },
  { tool: "write", input: { path: "a", content: 1 } },
  { tool: "write", input: { path: 1, content: "x" } },
  { tool: "edit", input: { path: "a", oldString: 1, newString: "b" } },
  { tool: "edit", input: { path: 1, oldString: "a", newString: "b" } },
  { tool: "patch", input: {} },
  { tool: "patch", input: { patchText: 5 } },
  { tool: "skill", input: {} },
  { tool: "skill", input: { id: 1 } },
  { tool: "grep", input: { path: 1, include: 1, literal: 1, caseSensitive: 1 } },
  { tool: "grep", input: { path: 1, include: 1, literal: 1, caseSensitive: 1, limit: 0 } },
  {
    tool: "grep",
    input: { pattern: "", path: 1, include: 1, literal: 1, caseSensitive: 1, limit: 0 },
  },
]

async function exportValidation() {
  const layer = appLayer("dev")
  const exported = await Effect.runPromise(
    Effect.gen(function* () {
      const directory = yield* tmpdirScoped()
      const locations = yield* LocationServiceMap.Service
      return yield* Effect.gen(function* () {
        const plugins = yield* Plugin.Service
        yield* plugins.awaitActivation
        const service = yield* Tool.Service
        const tools = (yield* service.list()).filter(
          (tool) => porter.has(tool.name) && tool.options?.namespace === undefined,
        )
        const byName = new Map(tools.map((tool) => [tool.name, tool]))
        const context = {
          sessionID: Session.ID.make("ses_validation"),
          agent: Agent.ID.make("build"),
          messageID: SessionMessage.ID.make("msg_validation"),
          id: Tool.CallID.make("call_validation"),
          progress: () => Effect.void,
        } satisfies Tool.Context
        const exported = []
        for (const item of validationCases) {
          const tool = byName.get(item.tool)
          if (tool === undefined) throw new Error(`corpus: missing tool ${item.tool}`)
          const exit = yield* executeTool(tool, item.input, context).pipe(Effect.exit)
          const failure = Exit.isFailure(exit) ? Cause.squash(exit.cause) : undefined
          exported.push({
            tool: item.tool,
            input: item.input,
            message: failure === undefined ? null : failure instanceof Error ? failure.message : String(failure),
          })
        }
        return exported
      }).pipe(
        Effect.scoped,
        Effect.provide(locations.get(Location.Ref.make({ directory: AbsolutePath.make(directory.path) }))),
      )
    }).pipe(Effect.scoped, Effect.provide(layer), Effect.provideService(Flag.Overrides, "-full_codemode")),
  )
  console.log(`__TOOL_CORPUS__${JSON.stringify(exported)}`)
}

interface TruncateCase {
  name: string
  limits: { maxLines: number; maxBytes: number }
  content: Tool.Content[]
  metadata?: Tool.Metadata
  failure?: boolean
}

const truncateCases: TruncateCase[] = [
  {
    name: "line-limit",
    limits: { maxLines: 2, maxBytes: 1_000 },
    content: [{ type: "text", text: "one\ntwo\nthree" }],
  },
  {
    name: "byte-limit-inside-line",
    limits: { maxLines: 100, maxBytes: 5 },
    content: [{ type: "text", text: "one\ntwo" }],
  },
  {
    name: "single-line-longer-than-byte-limit",
    limits: { maxLines: 100, maxBytes: 20 },
    content: [{ type: "text", text: JSON.stringify({ items: Array.from({ length: 50 }, (_, index) => index) }) }],
  },
  {
    name: "multibyte-check",
    limits: { maxLines: 100, maxBytes: 7 },
    content: [{ type: "text", text: "ab✓✓✓" }],
  },
  {
    name: "emoji-zwj",
    limits: { maxLines: 100, maxBytes: 7 },
    content: [{ type: "text", text: "ab👨‍👩‍👧‍👦cd" }],
  },
  {
    name: "emoji-modifier",
    limits: { maxLines: 100, maxBytes: 7 },
    content: [{ type: "text", text: "ab👍🏽cd" }],
  },
  {
    name: "emoji-flag",
    limits: { maxLines: 100, maxBytes: 7 },
    content: [{ type: "text", text: "ab🇯🇵cd" }],
  },
  {
    name: "astral-character",
    limits: { maxLines: 100, maxBytes: 7 },
    content: [{ type: "text", text: "ab𝒳𝒳" }],
  },
  {
    name: "long-flag-sequence",
    limits: { maxLines: 100, maxBytes: 165 },
    content: [{ type: "text", text: "a" + "🇯🇵".repeat(40) }],
  },
  {
    name: "long-grapheme-cluster",
    limits: { maxLines: 100, maxBytes: 165 },
    content: [{ type: "text", text: `ab${"👍" + "🏽\u0301".repeat(60)}z` }],
  },
  {
    name: "cut-inside-later-part",
    limits: { maxLines: 100, maxBytes: 6 },
    content: [
      { type: "text", text: "" },
      { type: "text", text: "abc" },
      { type: "text", text: "defgh" },
      { type: "text", text: "" },
    ],
  },
  {
    name: "cut-between-parts",
    limits: { maxLines: 100, maxBytes: 6 },
    content: [
      { type: "text", text: "abcde" },
      { type: "text", text: "fgh" },
    ],
  },
  {
    name: "crlf-line-cut",
    limits: { maxLines: 1, maxBytes: 1_000 },
    content: [{ type: "text", text: "one\r\ntwo\r\nthree" }],
  },
  {
    name: "blank-first-line",
    limits: { maxLines: 1, maxBytes: 1_000 },
    content: [{ type: "text", text: "\nfoo\nbar" }],
  },
  {
    name: "blank-last-preview-line",
    limits: { maxLines: 2, maxBytes: 1_000 },
    content: [{ type: "text", text: "a\n\n\nb" }],
  },
  {
    name: "zero-line-limit",
    limits: { maxLines: 0, maxBytes: 1_000 },
    content: [{ type: "text", text: "secret" }],
  },
  {
    name: "join-text-parts",
    limits: { maxLines: 2, maxBytes: 1_000 },
    content: [
      { type: "text", text: "one\n" },
      { type: "text", text: "two" },
      { type: "text", text: "three" },
    ],
  },
  {
    name: "mixed-content",
    limits: { maxLines: 2, maxBytes: 1_000 },
    content: [
      { type: "text", text: "before" },
      { type: "file", uri: "file:///image.png", mime: "image/png" },
      { type: "text", text: "after\nomitted" },
    ],
  },
  {
    name: "skip-reports-truncation",
    limits: { maxLines: 2_000, maxBytes: 50 * 1024 },
    content: [{ type: "text", text: "one\ntwo" }],
    metadata: { truncated: true, source: "tool" },
  },
  {
    name: "skip-reports-retention",
    limits: { maxLines: 2_000, maxBytes: 50 * 1024 },
    content: [{ type: "text", text: "one\ntwo" }],
    metadata: { truncated: false, source: "tool" },
  },
  {
    name: "fits",
    limits: { maxLines: 2_000, maxBytes: 50 * 1024 },
    content: [{ type: "text", text: "small" }],
  },
  {
    name: "trailing-newline",
    limits: { maxLines: 2, maxBytes: 1_000 },
    content: [{ type: "text", text: "one\ntwo\n" }],
  },
  {
    name: "trailing-newline-byte-limit",
    limits: { maxLines: 2, maxBytes: 3 },
    content: [{ type: "text", text: "one\n" }],
  },
  {
    name: "save-failure",
    limits: { maxLines: 2, maxBytes: 1_000 },
    content: [{ type: "text", text: "one\ntwo\nthree" }],
    failure: true,
  },
]

async function exportTruncate() {
  const exported = []
  for (const item of truncateCases) {
    const entry = await Effect.runPromise(
      Effect.gen(function* () {
        const directory = yield* tmpdirScoped()
        const layer = AppNodeBuilder.build(LayerNode.group([ToolOutput.node, FSUtil.node]), [
          Global.node.replace(Global.layerWith({ data: directory.path })),
        ])
        return yield* Effect.gen(function* () {
          const output = yield* ToolOutput.Service
          const fs = yield* FSUtil.Service
          yield* output.transform((editor) => editor.configure(item.limits))
          if (item.failure) yield* fs.writeFileString(`${directory.path}/${ToolOutput.DIRECTORY}`, "not a directory")
          const result = yield* output.truncate({
            content: item.content,
            ...(item.metadata === undefined ? {} : { metadata: item.metadata }),
          })
          const outputPath = result.metadata?.outputPath
          const saved =
            typeof outputPath === "string" && (yield* fs.exists(outputPath))
              ? yield* fs.readFileString(outputPath)
              : undefined
          const normalized = JSON.stringify(result)
            .replaceAll(typeof outputPath === "string" ? outputPath : "\u0000", "{{output}}")
            .replaceAll(directory.path, "{{data}}")
            .replace(/could not be saved: [^\]]*\]/g, "could not be saved: {{error}}]")
          return {
            name: item.name,
            limits: item.limits,
            content: item.content,
            ...(item.metadata === undefined ? {} : { metadata: item.metadata }),
            expected: {
              ...(JSON.parse(normalized) as object),
              ...(saved === undefined ? {} : { saved }),
            },
          }
        }).pipe(Effect.provide(layer))
      }).pipe(Effect.scoped),
    )
    exported.push(entry)
  }
  console.log(`__TOOL_CORPUS__${JSON.stringify(exported)}`)
}

type Part =
  | { text: string }
  | { repeat: { text: string; count: number } }
  | { padded: { count: number; width: number; separator?: string; final?: boolean; prefixAt?: { index: number; text: string } } }

type Fixture =
  | { path: string; kind?: "file"; parts: Part[] }
  | { path: string; kind: "binary"; base64: string }
  | { path: string; kind: "directory" }
  | { path: string; kind: "symlink"; target: string }
  | { path: string; kind: "truncate"; headerBase64?: string; size: number }

interface ReadFilesystemSpec {
  name: string
  fixtures?: Fixture[]
  path: string
  resource: string
  page?: { offset?: number; limit?: number }
}

function partText(parts: Part[]): string {
  return parts
    .map((part) => {
      if ("text" in part) return part.text
      if ("repeat" in part) return part.repeat.text.repeat(part.repeat.count)
      const padded = part.padded
      const lines = Array.from({ length: padded.count }, (_, index) => {
        const prefix = padded.prefixAt?.index === index ? padded.prefixAt.text : ""
        return prefix + String(index + 1).padEnd(padded.width, "x")
      })
      return lines.join(padded.separator ?? "\n") + (padded.final === false ? "" : (padded.separator ?? "\n"))
    })
    .join("")
}

async function materializeFixtures(root: string, outside: string, fixtures: Fixture[]) {
  for (const fixture of fixtures) {
    const target = fixture.path.replaceAll("{{root}}", root).replaceAll("{{outside}}", outside)
    await fs.mkdir(path.dirname(target), { recursive: true })
    if (fixture.kind === "directory") await fs.mkdir(target, { recursive: true })
    else if (fixture.kind === "binary") await fs.writeFile(target, Buffer.from(fixture.base64, "base64"))
    else if (fixture.kind === "symlink")
      await fs.symlink(fixture.target.replaceAll("{{root}}", root).replaceAll("{{outside}}", outside), target)
    else if (fixture.kind === "truncate") {
      await fs.writeFile(target, fixture.headerBase64 ? Buffer.from(fixture.headerBase64, "base64") : "")
      await fs.truncate(target, fixture.size)
    } else await fs.writeFile(target, partText(fixture.parts))
  }
}

function readFilesystemProgram(spec: ReadFilesystemSpec, root: string) {
  return Effect.gen(function* () {
    const spawner = yield* ChildProcessSpawner.ChildProcessSpawner
    const environment = Environment.makeFiles(Environment.makeLocalDriver(spawner))
    const target = spec.path.replaceAll("{{root}}", root)
    const exit = yield* ReadToolFileSystem.read(
      environment,
      AbsolutePath.make(target),
      spec.resource,
      spec.page ?? {},
    ).pipe(Effect.exit)
    if (Exit.isSuccess(exit)) {
      const result = exit.value
      const output =
        result.type === "media"
          ? {
              type: "media",
              uri: result.uri,
              mime: result.mime,
              name: result.name,
              bytesBase64: Buffer.from(result.bytes).toString("base64"),
            }
          : result
      const model = ReadTool.toModelContent(spec.resource, spec.page?.offset, result)
      return {
        ok: true,
        output,
        model: typeof model === "string" ? [{ type: "text", text: model }] : model,
      }
    }
    const error = Cause.squash(exit.cause)
    return {
      ok: false,
      kind: (error as { _tag?: string })?._tag ?? (error instanceof Error ? error.constructor.name : "unknown"),
      message: error instanceof Error ? error.message : String(error),
    }
  }).pipe(Effect.provide(LayerNode.compile(LayerNode.group([CrossSpawnSpawner.node, LayerNodePlatform.filesystem]))))
}

const textCase = (name: string, content: string, page: { offset?: number; limit?: number } = {}): ReadFilesystemSpec => ({
  name,
  fixtures: [{ path: "{{root}}/lines.txt", parts: [{ text: content }] }],
  path: "{{root}}/lines.txt",
  resource: "lines.txt",
  page,
})

const gif87 = Buffer.concat([
  Buffer.from("GIF87a"),
  Buffer.from("01000100800000000000ffffff2c00000000010001000002024401003b", "hex"),
]).toString("base64")
const gif89 = Buffer.from(gif87, "base64").toString("base64")

function readFilesystemSpecs(): ReadFilesystemSpec[] {
  return [
    textCase("reads a partial GIF signature as text", "GIF8 is text"),
    textCase("preserves a selected trailing blank line before continuation", "alpha\n\nomega\n", { offset: 1, limit: 2 }),
    textCase("preserves multiple selected trailing blank lines at a noninitial offset", "before\nalpha\n\n\nomega\n", {
      offset: 2,
      limit: 3,
    }),
    textCase("preserves a selected trailing blank line at EOF", "alpha\n\n", { limit: 2 }),
    textCase("preserves internal blank lines in a page", "alpha\n\nomega\n", { limit: 3 }),
    textCase("preserves continuation for a nonblank page", "alpha\n\nomega\n", { limit: 1 }),
    textCase("strips only the terminal file newline in a whole-file read", "alpha\n\n"),
    textCase("does not add a line for a whole-file terminal newline", "alpha\n"),
    textCase("preserves a whole-file read without a terminal newline", "alpha"),
    textCase("preserves empty whole-file output", ""),
    textCase("preserves empty-file page output", "", { limit: 2 }),
    textCase("numbers the only blank line in a newline-only file", "\n", { limit: 1 }),
    textCase("keeps the newline of a whole newline-only file", "\n"),
    textCase("numbers a single blank line before continuation", "alpha\n\nomega\n", { offset: 2, limit: 1 }),
    textCase("counts the last blank line when an offset is past EOF", "alpha\n\n", { offset: 9, limit: 1 }),
    {
      name: "applies the default line limit at physical line boundaries",
      fixtures: [
        {
          path: "{{root}}/dense.txt",
          parts: [{ repeat: { text: "x\r\n", count: 1_999 } }, { text: "last" }],
        },
      ],
      path: "{{root}}/dense.txt",
      resource: "dense.txt",
    },
    {
      name: "reports a continuation after two thousand lines",
      fixtures: [
        {
          path: "{{root}}/dense.txt",
          parts: [{ repeat: { text: "x\r\n", count: 1_999 } }, { text: "\r\nlast" }],
        },
      ],
      path: "{{root}}/dense.txt",
      resource: "dense.txt",
    },
    {
      name: "applies the byte limit to the returned content of small files",
      fixtures: [
        {
          path: "{{root}}/bytes.txt",
          parts: [{ repeat: { text: `${"é".repeat(799)}x\n`, count: 32 } }],
        },
      ],
      path: "{{root}}/bytes.txt",
      resource: "bytes.txt",
    },
    {
      name: "truncates at the byte limit with continuation",
      fixtures: [
        {
          path: "{{root}}/bytes.txt",
          parts: [{ repeat: { text: `${"x".repeat(2_001)}\n`, count: 25 } }, { repeat: { text: "é", count: 500 } }],
        },
      ],
      path: "{{root}}/bytes.txt",
      resource: "bytes.txt",
    },
    {
      name: "reads an extensionless GIF as an image",
      fixtures: [{ path: "{{root}}/pixel", kind: "binary", base64: gif87 }],
      path: "{{root}}/pixel",
      resource: "pixel",
    },
    {
      name: "reads a renamed GIF as an image",
      fixtures: [{ path: "{{root}}/pixel.txt", kind: "binary", base64: gif89 }],
      path: "{{root}}/pixel.txt",
      resource: "pixel.txt",
    },
    { name: "preserves the environment not-found error", path: "{{root}}/missing.txt", resource: "missing.txt" },
    {
      name: "returns a listing when read reports a directory",
      fixtures: [
        { path: "{{root}}/folder", kind: "directory" },
        { path: "{{root}}/file.txt", parts: [{ text: "hello" }] },
      ],
      path: "{{root}}",
      resource: "folder",
    },
    {
      name: "reads malformed UTF-8 lossily",
      fixtures: [{ path: "{{root}}/malformed.txt", kind: "binary", base64: Buffer.from([0x68, 0x69, 0x80]).toString("base64") }],
      path: "{{root}}/malformed.txt",
      resource: "malformed.txt",
    },
    {
      name: "rejects null-byte binary content",
      fixtures: [{ path: "{{root}}/archive.dat", kind: "binary", base64: Buffer.from([0, 1, 2, 3]).toString("base64") }],
      path: "{{root}}/archive.dat",
      resource: "archive.dat",
    },
    {
      name: "reads text despite a binary-associated extension",
      fixtures: [{ path: "{{root}}/notes.docx", parts: [{ text: "plain text" }] }],
      path: "{{root}}/notes.docx",
      resource: "notes.docx",
    },
    {
      name: "lists unresolved symlinks",
      fixtures: [
        { path: "{{root}}/folder", kind: "directory" },
        { path: "{{root}}/file.txt", parts: [{ text: "hello" }] },
        { path: "{{root}}/escape", kind: "symlink", target: "{{outside}}/target.txt" },
        { path: "{{root}}/broken", kind: "symlink", target: "{{root}}/missing.txt" },
      ],
      path: "{{root}}",
      resource: "folder",
    },
    {
      name: "reads a symlinked directory as a listing",
      fixtures: [
        { path: "{{root}}/target", kind: "directory" },
        { path: "{{root}}/target/file.txt", parts: [{ text: "hello" }] },
        { path: "{{root}}/link", kind: "symlink", target: "{{root}}/target" },
      ],
      path: "{{root}}/link",
      resource: "link",
    },
    {
      name: "returns the last lines when the offset is past the end",
      fixtures: [{ path: "{{root}}/short.txt", parts: [{ text: "one\ntwo\nthree\nfour\nfive\n" }] }],
      path: "{{root}}/short.txt",
      resource: "short.txt",
      page: { offset: 9, limit: 2 },
    },
    {
      name: "returns no lines for an empty file with a past-end offset",
      fixtures: [{ path: "{{root}}/empty.txt", parts: [{ text: "" }] }],
      path: "{{root}}/empty.txt",
      resource: "empty.txt",
      page: { offset: 5 },
    },
    {
      name: "caps a past-end tail at the byte limit",
      fixtures: [
        { path: "{{root}}/long.txt", parts: [{ padded: { count: 1_800, width: 1_000, final: false } }] },
      ],
      path: "{{root}}/long.txt",
      resource: "long.txt",
      page: { offset: 2_000, limit: 2_000 },
    },
    {
      name: "returns multi-chunk past-end lines",
      fixtures: [{ path: "{{root}}/lf.txt", parts: [{ padded: { count: 3_000, width: 100 } }] }],
      path: "{{root}}/lf.txt",
      resource: "lf.txt",
      page: { offset: 5_000 },
    },
    {
      name: "keeps a mid-file byte order mark as line content",
      fixtures: [
        {
          path: "{{root}}/bom.txt",
          parts: [{ padded: { count: 3_000, width: 100, final: false, prefixAt: { index: 2_900, text: "\uFEFF" } } }],
        },
      ],
      path: "{{root}}/bom.txt",
      resource: "bom.txt",
      page: { offset: 3_001 },
    },
    {
      name: "truncates a huge last line in a past-end tail",
      fixtures: [
        {
          path: "{{root}}/huge-last.txt",
          parts: [
            { padded: { count: 10, width: 100 } },
            { repeat: { text: "z", count: 300 * 1024 } },
          ],
        },
      ],
      path: "{{root}}/huge-last.txt",
      resource: "huge-last.txt",
      page: { offset: 12 },
    },
    {
      name: "caps a wide past-end tail at the byte limit",
      fixtures: [{ path: "{{root}}/wide.txt", parts: [{ padded: { count: 200, width: 3_000 } }] }],
      path: "{{root}}/wide.txt",
      resource: "wide.txt",
      page: { offset: 201 },
    },
    {
      name: "starts a negative offset that many lines before the end",
      fixtures: [{ path: "{{root}}/small.txt", parts: [{ text: "one\ntwo\nthree\nfour\nfive\n" }] }],
      path: "{{root}}/small.txt",
      resource: "small.txt",
      page: { offset: -3 },
    },
    {
      name: "limits a negative offset with continuation",
      fixtures: [{ path: "{{root}}/limited.txt", parts: [{ text: "one\ntwo\nthree\nfour\nfive\n" }] }],
      path: "{{root}}/limited.txt",
      resource: "limited.txt",
      page: { offset: -4, limit: 2 },
    },
    {
      name: "starts a negative offset from the end of a multi-chunk file",
      fixtures: [{ path: "{{root}}/lf.txt", parts: [{ padded: { count: 3_000, width: 100 } }] }],
      path: "{{root}}/lf.txt",
      resource: "lf.txt",
      page: { offset: -50 },
    },
    {
      name: "limits a negative offset window in a multi-chunk file",
      fixtures: [{ path: "{{root}}/window.txt", parts: [{ padded: { count: 3_000, width: 100 } }] }],
      path: "{{root}}/window.txt",
      resource: "window.txt",
      page: { offset: -50, limit: 10 },
    },
    {
      name: "stops a negative whole read at the byte limit",
      fixtures: [{ path: "{{root}}/whole.txt", parts: [{ padded: { count: 3_000, width: 100 } }] }],
      path: "{{root}}/whole.txt",
      resource: "whole.txt",
      page: { offset: -5_000 },
    },
    {
      name: "caps a wide negative offset at the byte limit",
      fixtures: [{ path: "{{root}}/wide.txt", parts: [{ padded: { count: 200, width: 3_000 } }] }],
      path: "{{root}}/wide.txt",
      resource: "wide.txt",
      page: { offset: -100 },
    },
    {
      name: "reads the end of a file many chunks long",
      fixtures: [{ path: "{{root}}/negative.txt", parts: [{ padded: { count: 60_000, width: 50 } }] }],
      path: "{{root}}/negative.txt",
      resource: "negative.txt",
      page: { offset: -5 },
    },
    {
      name: "reads a deep page of a file many chunks long",
      fixtures: [{ path: "{{root}}/deep.txt", parts: [{ padded: { count: 60_000, width: 50 } }] }],
      path: "{{root}}/deep.txt",
      resource: "deep.txt",
      page: { offset: 45_000, limit: 2 },
    },
    {
      name: "reads a past-end tail of a file many chunks long",
      fixtures: [{ path: "{{root}}/past.txt", parts: [{ padded: { count: 60_000, width: 50 } }] }],
      path: "{{root}}/past.txt",
      resource: "past.txt",
      page: { offset: 70_000, limit: 3 },
    },
    {
      name: "rejects a negative offset for a directory",
      fixtures: [{ path: "{{root}}/a.txt", parts: [{ text: "a\n" }] }],
      path: "{{root}}",
      resource: "src",
      page: { offset: -1 },
    },
    {
      name: "pages text with one-based offsets",
      fixtures: [{ path: "{{root}}/lines.txt", parts: [{ text: "one\r\ntwo\nthree" }] }],
      path: "{{root}}/lines.txt",
      resource: "lines.txt",
      page: { offset: 2, limit: 1 },
    },
    {
      name: "truncates long lines",
      fixtures: [{ path: "{{root}}/long.txt", parts: [{ text: `${"a".repeat(1_999)}😀tail` }] }],
      path: "{{root}}/long.txt",
      resource: "long.txt",
      page: { limit: 1 },
    },
    {
      name: "enforces the line budget with a continuation offset",
      fixtures: [
        {
          path: "{{root}}/many-lines.txt",
          parts: [{ padded: { count: 2_001, width: 0, final: false } }],
        },
      ],
      path: "{{root}}/many-lines.txt",
      resource: "many-lines.txt",
      page: { limit: 2_000 },
    },
    {
      name: "enforces the byte budget with a continuation offset",
      fixtures: [
        {
          path: "{{root}}/many-bytes.txt",
          parts: [{ repeat: { text: `${"a".repeat(2_000)}\n`, count: 199 } }, { text: "a".repeat(2_000) }],
        },
      ],
      path: "{{root}}/many-bytes.txt",
      resource: "many-bytes.txt",
    },
    {
      name: "sorts and pages directory entries",
      fixtures: [
        { path: "{{root}}/z", kind: "directory" },
        { path: "{{root}}/a", kind: "directory" },
        { path: "{{root}}/b.txt", parts: [{ text: "" }] },
      ],
      path: "{{root}}",
      resource: "folder",
      page: { offset: 2, limit: 1 },
    },
    {
      name: "stops checking for null bytes after the requested page",
      fixtures: [
        {
          path: "{{root}}/nul.txt",
          kind: "binary",
          base64: Buffer.from([...Buffer.from("one\n"), 0]).toString("base64"),
        },
      ],
      path: "{{root}}/nul.txt",
      resource: "nul.txt",
      page: { limit: 1 },
    },
    {
      name: "rejects a null byte before the requested page",
      fixtures: [
        {
          path: "{{root}}/nul-prefix.txt",
          kind: "binary",
          base64: Buffer.from([...Buffer.from("one"), 0, 10, 116, 119, 111, 10]).toString("base64"),
        },
      ],
      path: "{{root}}/nul-prefix.txt",
      resource: "nul-prefix.txt",
      page: { offset: 2, limit: 1 },
    },
    {
      name: "ignores a null byte after a chunked page but rejects it for a past-end offset",
      fixtures: [
        {
          path: "{{root}}/nul.txt",
          parts: [{ repeat: { text: "x", count: 256 * 1024 } }, { text: "\nselected\n\0\ntail" }],
        },
      ],
      path: "{{root}}/nul.txt",
      resource: "nul.txt",
      page: { offset: 2, limit: 1 },
    },
    {
      name: "rejects a null byte after a chunked past-end offset",
      fixtures: [
        {
          path: "{{root}}/nul.txt",
          parts: [{ repeat: { text: "x", count: 256 * 1024 } }, { text: "\nselected\n\0\ntail" }],
        },
      ],
      path: "{{root}}/nul.txt",
      resource: "nul.txt",
      page: { offset: 5, limit: 1 },
    },
    {
      name: "keeps a split byte order mark in a past-end tail",
      fixtures: [
        {
          path: "{{root}}/marked.txt",
          parts: [{ repeat: { text: "x", count: 256 * 1024 - 2 } }, { text: "\n\uFEFFselected" }],
        },
      ],
      path: "{{root}}/marked.txt",
      resource: "marked.txt",
      page: { offset: 3, limit: 1 },
    },
    {
      name: "strips a split byte order mark at a negative offset",
      fixtures: [
        {
          path: "{{root}}/marked.txt",
          parts: [{ repeat: { text: "x", count: 256 * 1024 - 2 } }, { text: "\n\uFEFFselected" }],
        },
      ],
      path: "{{root}}/marked.txt",
      resource: "marked.txt",
      page: { offset: -1, limit: 1 },
    },
    {
      name: "reads page two after fetching more than the first 256KB range",
      fixtures: [
        {
          path: "{{root}}/large.txt",
          parts: [{ repeat: { text: "a", count: 300 * 1024 } }, { text: "\nsecond\n" }],
        },
      ],
      path: "{{root}}/large.txt",
      resource: "large.txt",
      page: { offset: 2, limit: 1 },
    },
    {
      name: "reads after a newline at the first chunk boundary",
      fixtures: [
        {
          path: "{{root}}/boundary.txt",
          parts: [{ repeat: { text: "a", count: 256 * 1024 - 1 } }, { text: "\nsecond\n" }],
        },
      ],
      path: "{{root}}/boundary.txt",
      resource: "boundary.txt",
      page: { offset: 2, limit: 1 },
    },
    {
      name: "preserves the media ingestion limit message",
      fixtures: [
        {
          path: "{{root}}/oversized.png",
          kind: "truncate",
          headerBase64: Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]).toString("base64"),
          size: 20 * 1024 * 1024 + 1,
        },
      ],
      path: "{{root}}/oversized.png",
      resource: "oversized.png",
    },
    {
      name: "reads PDFs as bounded media",
      fixtures: [{ path: "{{root}}/document.pdf", parts: [{ text: "%PDF-1.7\ncontent" }] }],
      path: "{{root}}/document.pdf",
      resource: "document.pdf",
    },
  ]
}

async function exportReadFilesystem() {
  const base = await fs.mkdtemp(path.join(os.tmpdir(), "opencode-tool-corpus-"))
  const exported: Array<Record<string, unknown>> = []
  try {
    for (const spec of readFilesystemSpecs()) {
      const root = path.join(base, spec.name.replace(/[^a-zA-Z0-9]+/g, "-"))
      const outside = `${root}-outside`
      await fs.mkdir(root, { recursive: true })
      await fs.mkdir(outside, { recursive: true })
      if (process.platform !== "win32") await materializeFixtures(root, outside, spec.fixtures ?? [])
      const exit = await Effect.runPromiseExit(readFilesystemProgram(spec, root))
      await fs.rm(root, { recursive: true, force: true })
      await fs.rm(outside, { recursive: true, force: true })
      if (Exit.isFailure(exit)) {
        console.error(`corpus: read_filesystem ${spec.name} failed`)
        console.error(exit.cause)
        process.exit(1)
      }
      const normalized = JSON.stringify({ ...spec, expected: exit.value })
        .replaceAll(outside, "{{outside}}")
        .replaceAll(root, "{{root}}")
      exported.push(JSON.parse(normalized))
    }
  } finally {
    await fs.rm(base, { recursive: true, force: true })
  }
  console.log(`__TOOL_CORPUS__${JSON.stringify(exported)}`)
}

const readToolNode = makeLocationNode({
  name: "corpus/read-tool-plugin",
  layer: Layer.effectDiscard(registerToolPlugin(ReadTool.Plugin)),
  deps: [
    Tool.node,
    FileAccess.node,
    Image.node,
    Permission.node,
    SessionInstructions.node,
    InstructionDiscovery.node,
    Skill.node,
    FSUtil.node,
    Environment.node,
    Location.node,
    Config.node,
    Flag.node,
  ],
})
const corpusConfig = Config.testLayer()
const corpusImage = AppNodeBuilder.build(Image.node)

interface SkillFixture {
  id: string
  name: string
  path: string
  content: string
  files?: Record<string, string>
}

interface ReadCase {
  name: string
  fixtures?: Fixture[]
  call: { path: string; offset?: number; limit?: number }
  permission?: { allow?: boolean; deniedResources?: string[] }
  skills?: SkillFixture[]
}

const readCase = (
  name: string,
  fixtures: Fixture[],
  call: ReadCase["call"],
  extra: Partial<ReadCase> = {},
): ReadCase => ({ name, fixtures, call, ...extra })

const guideSkill: SkillFixture = {
  id: "guide",
  name: "Guide",
  path: "skill://guide/SKILL.md",
  content: "# Guide\nRead references/style.md",
  files: { "references/style.md": "Line 1\nLine 2", "references/guide.pdf": "%PDF-1.7\nguide" },
}

const skillCases = (): ReadCase[] => [
  readCase("reads virtual skill files", [], { path: "skill://guide/references/style.md", offset: 2, limit: 1 }, {
    skills: [guideSkill],
  }),
  readCase("lists a virtual skill directory", [], { path: "skill://guide/" }, { skills: [guideSkill] }),
  readCase("returns a missing virtual skill file", [], { path: "skill://guide/missing.md" }, { skills: [guideSkill] }),
  readCase("rejects an unsafe virtual skill path", [], { path: "skill://guide/../SKILL.md" }, { skills: [guideSkill] }),
  readCase("rejects a bare virtual skill URI", [], { path: "skill://" }),
  readCase("rejects a disk-backed skill path", [], { path: "skill://disk/SKILL.md" }, {
    skills: [{ id: "disk", name: "Disk", path: "/somewhere/disk/SKILL.md", content: "Disk" }],
  }),
  readCase("returns a virtual skill PDF as media", [], { path: "skill://guide/references/guide.pdf" }, {
    skills: [guideSkill],
  }),
  readCase("denies a virtual skill before reading", [], { path: "skill://guide/SKILL.md" }, {
    skills: [guideSkill],
    permission: { deniedResources: ["guide"] },
  }),
  readCase("denies a virtual skill file after the skill check", [], { path: "skill://guide/SKILL.md" }, {
    skills: [guideSkill],
    permission: { deniedResources: ["skill://guide/SKILL.md"] },
  }),
  readCase("canonicalizes a trailing slash before the read check", [], { path: "skill://guide/SKILL.md/" }, {
    skills: [guideSkill],
    permission: { deniedResources: ["skill://guide/SKILL.md"] },
  }),
]

function readSpecs(): ReadCase[] {
  return [
    readCase("reads and authorizes through the location filesystem", [
      { path: "{{root}}/README.md", parts: [{ text: "hello" }] },
    ], { path: "README.md" }),
    readCase("rejects a zero limit before reading", [
      { path: "{{root}}/README.md", parts: [{ text: "hello" }] },
    ], { path: "README.md", limit: 0 }),
    readCase("does not read when permission is denied", [
      { path: "{{root}}/README.md", parts: [{ text: "hello" }] },
    ], { path: "README.md" }, { permission: { allow: false } }),
    readCase("asks for external approval before reading an external path", [
      { path: "{{outside}}/notes.txt", parts: [{ text: "hello" }] },
    ], { path: "{{outside}}/notes.txt" }),
    readCase("returns a small PNG as native media", [
      {
        path: "{{root}}/pixel.png",
        kind: "binary",
        base64: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=",
      },
    ], { path: "pixel.png" }),
    readCase("returns a supported image despite a misleading extension", [
      {
        path: "{{root}}/pixel.bin",
        kind: "binary",
        base64: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=",
      },
    ], { path: "pixel.bin" }),
    readCase("returns PDFs as native media", [
      { path: "{{root}}/document.pdf", parts: [{ text: "%PDF-1.7\ncontent" }] },
    ], { path: "document.pdf" }),
    readCase("returns filesystem failures to the model", [
      { path: "{{root}}/archive.dat", kind: "binary", base64: Buffer.from("one\narchive\u0000\n").toString("base64") },
    ], { path: "archive.dat", offset: 2, limit: 1 }),
    readCase("returns missing paths with ranked suggestions", [
      { path: "{{root}}/__missing_read_target__.txt.bak", parts: [{ text: "x\n" }] },
      { path: "{{root}}/copy___missing_read_target__.txt", parts: [{ text: "x\n" }] },
      { path: "{{root}}/old___missing_read_target__.txt", parts: [{ text: "x\n" }] },
      { path: "{{root}}/other___missing_read_target__.txt", parts: [{ text: "x\n" }] },
      { path: "{{root}}/unrelated.txt", parts: [{ text: "x\n" }] },
    ], { path: "__missing_read_target__.txt" }),
    readCase("names the first missing folder", [
      { path: "{{root}}/__missing_read_folder__-old", kind: "directory" },
    ], { path: "__missing_read_folder__/src/file.ts" }),
    readCase("trusts the environment for which folders exist", [
      { path: "{{root}}/__only_in_workspace__/file.tsx", parts: [{ text: "x\n" }] },
    ], { path: "{{root}}/__only_in_workspace__/file.ts" }),
    readCase("ranks same-folder suggestions and shows at most three", [
      { path: "{{root}}/acnfig.ts", parts: [{ text: "x\n" }] },
      { path: "{{root}}/cconfig.ts", parts: [{ text: "x\n" }] },
      { path: "{{root}}/config.json", parts: [{ text: "x\n" }] },
      { path: "{{root}}/Config.ts", parts: [{ text: "x\n" }] },
    ], { path: "config.ts" }),
    readCase("recovers a unique file whose non-breaking spaces differ", [
      { path: "{{root}}/Screenshot 2026-08-27 3.15\u202fPM.png", parts: [{ text: "hello" }] },
    ], { path: "Screenshot 2026-08-27 3.15 PM.png" }),
    readCase("recovers an external filename without repeating directory approval", [
      { path: "{{outside}}/report\u202ffinal.txt", parts: [{ text: "hello" }] },
    ], { path: "{{outside}}/report final.txt" }),
    readCase("does not read a recovered filename denied by its own read rules", [
      { path: "{{root}}/report\u202ffinal.txt", parts: [{ text: "hello" }] },
    ], { path: "{{root}}/report final.txt" }, { permission: { deniedResources: ["report\u202ffinal.txt"] } }),
    readCase("does not recover ambiguous files", [
      { path: "{{root}}/report\u00a0final.txt", parts: [{ text: "hello" }] },
      { path: "{{root}}/report\u202ffinal.txt", parts: [{ text: "hello" }] },
    ], { path: "report final.txt" }),
    readCase("does not recover a directory", [
      { path: "{{root}}/report\u00a0final", kind: "directory" },
    ], { path: "report final" }),
    readCase("lists a bounded directory page", [
      { path: "{{root}}/src/assets", kind: "directory" },
      { path: "{{root}}/src/components", kind: "directory" },
      { path: "{{root}}/src/index.ts", parts: [{ text: "x\n" }] },
      { path: "{{root}}/src/z1.ts", parts: [{ text: "x\n" }] },
      { path: "{{root}}/src/z2.ts", parts: [{ text: "x\n" }] },
    ], { path: "src", offset: 2, limit: 2 }),
    readCase("normalizes a zero directory offset", [
      { path: "{{root}}/src/index.ts", parts: [{ text: "x\n" }] },
      { path: "{{root}}/src/zzz.ts", parts: [{ text: "x\n" }] },
    ], { path: "src", offset: 0, limit: 1 }),
    readCase("forwards pagination and returns a bounded text page", [
      { path: "{{root}}/large.txt", parts: [{ text: "a\nhello\nc\n" }] },
    ], { path: "large.txt", offset: 2, limit: 1 }),
    readCase("forwards a negative offset and shows real line numbers", [
      {
        path: "{{root}}/log.txt",
        parts: [
          {
            text: Array.from({ length: 13 }, (_, index) => (index === 8 ? "nine" : index === 9 ? "ten" : `line ${index + 1}`)).join(
              "\n",
            ),
          },
        ],
      },
    ], { path: "log.txt", offset: -5, limit: 2 }),
    readCase("returns the negative directory offset error", [
      { path: "{{root}}/src", kind: "directory" },
    ], { path: "src", offset: -5 }),
    readCase("discovers nearby instructions up to the location root", [
      { path: "{{root}}/AGENTS.md", parts: [{ text: "root" }] },
      { path: "{{root}}/packages/AGENTS.md", parts: [{ text: "packages" }] },
      { path: "{{root}}/packages/CLAUDE.md", parts: [{ text: "claude" }] },
      { path: "{{root}}/packages/src/file.ts", parts: [{ text: "hello" }] },
    ], { path: "packages/src/file.ts" }),
    readCase("does not load the file being read as nearby instructions", [
      { path: "{{root}}/packages/AGENTS.md", parts: [{ text: "packages" }] },
    ], { path: "packages/AGENTS.md" }),
    ...skillCases(),
  ]
}

function readLayerFor(root: string, permission: Layer.Layer<Permission.Service>, sessions: Layer.Layer<SessionInstructions.Service>) {
  return Layer.mergeAll(
    AppNodeBuilder.build(LayerNode.group([Tool.node, readToolNode, Skill.node]), [
      Environment.node.replace(hostEnvironmentLayer),
      Permission.node.replace(permission),
      Config.node.replace(corpusConfig),
      Image.node.replace(corpusImage),
      Location.node.replace(
        Layer.succeed(Location.Service, Location.Service.of(location({ directory: AbsolutePath.make(root) }))),
      ),
      Global.node.replace(Global.layerWith({ data: Global.Path.data })),
      SessionInstructions.node.replace(sessions),
    ]),
    corpusConfig,
    corpusImage,
  ).pipe(Layer.provide(Layer.succeed(Flag.Overrides, undefined)))
}

interface Assertion {
  action: string
  resources: string[]
  save?: string[]
}

async function fileTree(root: string, outside: string) {
  const tree: Record<string, string | null> = {}
  const walk = async (directory: string, prefix: string) => {
    for (const entry of await fs.readdir(directory, { withFileTypes: true })) {
      const absolute = path.join(directory, entry.name)
      const key = `${prefix}/${entry.name}`
      if (entry.isDirectory()) {
        tree[key] = null
        await walk(absolute, key)
      } else if (entry.isSymbolicLink()) tree[key] = `symlink:${await fs.readlink(absolute)}`
      else tree[key] = (await fs.readFile(absolute)).toString("base64")
    }
  }
  await walk(root, "{{root}}")
  await walk(outside, "{{outside}}")
  return tree
}

async function exportRead() {
  const base = await fs.mkdtemp(path.join(os.tmpdir(), "opencode-tool-corpus-"))
  const exported: Array<Record<string, unknown>> = []
  try {
    for (const [index, spec] of readSpecs().entries()) {
      const root = path.join(base, `${index}-${spec.name.replace(/[^a-zA-Z0-9]+/g, "-").slice(0, 40)}`)
      const outside = `${root}-outside`
      await fs.mkdir(root, { recursive: true })
      await fs.mkdir(outside, { recursive: true })
      await materializeFixtures(root, outside, spec.fixtures ?? [])
      const assertions: Assertion[] = []
      const loaded: string[][] = []
      const allow = spec.permission?.allow ?? true
      const denied = spec.permission?.deniedResources ?? []
      const allowed = (input: { resources: readonly string[] }) =>
        allow && !input.resources.some((resource) => denied.includes(resource))
      const permission = permissionLayer({
        assert: (input) =>
          Effect.sync(() => {
            assertions.push({
              action: input.action,
              resources: [...input.resources],
              ...(input.save === undefined ? {} : { save: [...input.save] }),
            })
          }).pipe(
            Effect.andThen(
              allowed(input)
                ? Effect.void
                : Effect.fail(
                    new Permission.BlockedError({
                      rules: [],
                      permission: input.action,
                      resources: input.resources,
                    }),
                  ),
            ),
          ),
        allows: (input) => Effect.succeed(allowed(input)),
      })
      const sessions = Layer.succeed(
        SessionInstructions.Service,
        SessionInstructions.Service.of({
          load: (input) => Effect.sync(() => void loaded.push([...input.paths])),
        }),
      )
      const call = {
        path: spec.call.path.replaceAll("{{root}}", root).replaceAll("{{outside}}", outside),
        ...(spec.call.offset === undefined ? {} : { offset: spec.call.offset }),
        ...(spec.call.limit === undefined ? {} : { limit: spec.call.limit }),
      }
      const program = Effect.gen(function* () {
        const registry = yield* Tool.Service
        if (spec.skills !== undefined) {
          const skills = yield* Skill.Service
          for (const fixture of spec.skills)
            yield* skills.transform((editor) =>
              editor.add(
                Skill.Info.make({
                  id: Skill.ID.make(fixture.id),
                  name: Skill.Name.make(fixture.name),
                  description: fixture.name,
                  path: AbsolutePath.make(fixture.path),
                  content: fixture.content,
                }),
                fixture.files,
              ),
            )
        }
        return yield* (yield* registry.snapshot()).execute({
          sessionID: Session.ID.make("ses_read_tool_test"),
          ...toolIdentity,
          call: { type: "tool-call", id: `call-${index}`, name: "read", input: call },
        }).pipe(
          Effect.map((result) => ({ status: "completed" as const, ...result })),
          Effect.catchTag("Tool.Error", (error) =>
            Effect.succeed({ status: "error" as const, error: toSessionError(error) }),
          ),
        )
      }).pipe(Effect.scoped, Effect.provide(readLayerFor(root, permission, sessions)))
      const exit = await Effect.runPromiseExit(program)
      if (Exit.isFailure(exit)) {
        console.error(`corpus: read ${spec.name} failed`)
        console.error(exit.cause)
        process.exit(1)
      }
      const execution = exit.value
      const tree = await fileTree(root, outside)
      const expected: Record<string, unknown> = { status: execution.status }
      if (execution.status === "completed") {
        expected.output = execution.output
        expected.content = execution.content
        expected.metadata = execution.metadata
      } else expected.error = execution.error?.message
      expected.assertions = assertions
      expected.loaded = loaded
      expected.tree = tree
      const normalized = JSON.stringify({ ...spec, fixtures: spec.fixtures ?? [], call, expected })
        .replaceAll(outside, "{{outside}}")
        .replaceAll(root, "{{root}}")
      exported.push(JSON.parse(normalized))
      await fs.rm(root, { recursive: true, force: true })
      await fs.rm(outside, { recursive: true, force: true })
    }
  } finally {
    await fs.rm(base, { recursive: true, force: true })
  }
  console.log(`__TOOL_CORPUS__${JSON.stringify(exported)}`)
}

const writeToolNode = makeLocationNode({
  name: "corpus/write-tool-plugin",
  layer: Layer.effectDiscard(registerToolPlugin(WriteTool.Plugin)),
  deps: [Tool.node, FileWrite.node, Config.node, Flag.node],
})

const editToolNode = makeLocationNode({
  name: "corpus/edit-tool-plugin",
  layer: Layer.effectDiscard(registerToolPlugin(EditTool.Plugin)),
  deps: [
    Tool.node,
    FileWrite.node,
    FileMutation.node,
    Environment.node,
    Location.node,
    Permission.node,
    Config.node,
    Flag.node,
  ],
})

const patchToolNode = makeLocationNode({
  name: "corpus/patch-tool-plugin",
  layer: Layer.effectDiscard(registerToolPlugin(PatchTool.Plugin)),
  deps: [
    Tool.node,
    FileAccess.node,
    FileMutation.node,
    Environment.node,
    Formatter.node,
    Location.node,
    Permission.node,
    Config.node,
    Flag.node,
  ],
})

const corpusFormatter = Layer.mock(Formatter.Service, { file: () => Effect.succeed(false) })

interface LocationSpec {
  directory?: string
  projectDirectory?: string
}

function toolLayerFor(
  tool: "write" | "edit" | "patch",
  root: string,
  permission: Layer.Layer<Permission.Service>,
  place: LocationSpec = {},
) {
  const directory = AbsolutePath.make(place.directory?.replaceAll("{{root}}", root) ?? root)
  const projectDirectory = AbsolutePath.make(place.projectDirectory?.replaceAll("{{root}}", root) ?? directory)
  return AppNodeBuilder.build(
    LayerNode.group([
      Tool.node,
      FileAccess.node,
      FileMutation.node,
      tool === "write" ? writeToolNode : tool === "edit" ? editToolNode : patchToolNode,
    ]),
    [
      Environment.node.replace(hostEnvironmentLayer),
      Permission.node.replace(permission),
      Config.node.replace(corpusConfig),
      Formatter.node.replace(corpusFormatter),
      Location.node.replace(
        Layer.succeed(Location.Service, Location.Service.of(location({ directory }, { projectDirectory }))),
      ),
    ],
  ).pipe(Layer.provide(Layer.succeed(Flag.Overrides, undefined)))
}

interface ToolPermission {
  allow?: boolean
  deniedResources?: string[]
  deniedActions?: string[]
  readAllowed?: boolean
}

interface ToolCase {
  name: string
  fixtures?: Fixture[]
  call: Record<string, unknown>
  permission?: ToolPermission
  location?: LocationSpec
}

async function exportToolCases(tool: "write" | "edit" | "patch", specs: ToolCase[]) {
  const base = await fs.mkdtemp(path.join(os.tmpdir(), "opencode-tool-corpus-"))
  const exported: Array<Record<string, unknown>> = []
  try {
    for (const [index, spec] of specs.entries()) {
      const root = path.join(base, `${index}-${spec.name.replace(/[^a-zA-Z0-9]+/g, "-").slice(0, 40)}`)
      const outside = `${root}-outside`
      await fs.mkdir(root, { recursive: true })
      await fs.mkdir(outside, { recursive: true })
      await materializeFixtures(root, outside, spec.fixtures ?? [])
      const assertions: Assertion[] = []
      const allow = spec.permission?.allow ?? true
      const deniedResources = spec.permission?.deniedResources ?? []
      const deniedActions = spec.permission?.deniedActions ?? []
      const readAllowed = spec.permission?.readAllowed ?? true
      const allowed = (input: { action: string; resources: readonly string[] }) =>
        allow &&
        !deniedActions.includes(input.action) &&
        !input.resources.some((resource) => deniedResources.includes(resource))
      const permission = permissionLayer({
        assert: (input) =>
          Effect.sync(() => {
            assertions.push({
              action: input.action,
              resources: [...input.resources],
              ...(input.save === undefined ? {} : { save: [...input.save] }),
              ...(input.metadata === undefined ? {} : { metadata: input.metadata }),
            })
          }).pipe(
            Effect.andThen(
              allowed(input)
                ? Effect.void
                : Effect.fail(
                    new Permission.BlockedError({
                      rules: [],
                      permission: input.action,
                      resources: input.resources,
                    }),
                  ),
            ),
          ),
        allows: (input) => Effect.succeed(input.action === "read" ? readAllowed : allowed(input)),
      })
      const call = Object.fromEntries(
        Object.entries(spec.call).map(([key, value]) => [
          key,
          typeof value === "string" ? value.replaceAll("{{root}}", root).replaceAll("{{outside}}", outside) : value,
        ]),
      )
      const program = Effect.gen(function* () {
        const registry = yield* Tool.Service
        return yield* (yield* registry.snapshot()).execute({
          sessionID: Session.ID.make(`ses_${tool}_tool_test`),
          ...toolIdentity,
          call: { type: "tool-call", id: `call-${index}`, name: tool, input: call },
        }).pipe(
          Effect.map((result) => ({ status: "completed" as const, ...result })),
          Effect.catchTag("Tool.Error", (error) =>
            Effect.succeed({ status: "error" as const, error: toSessionError(error) }),
          ),
        )
      }).pipe(Effect.scoped, Effect.provide(toolLayerFor(tool, root, permission, spec.location)))
      const exit = await Effect.runPromiseExit(program)
      if (Exit.isFailure(exit)) {
        console.error(`corpus: ${tool} ${spec.name} failed`)
        console.error(exit.cause)
        process.exit(1)
      }
      const execution = exit.value
      const tree = await fileTree(root, outside)
      const expected: Record<string, unknown> = { status: execution.status }
      if (execution.status === "completed") {
        expected.output = execution.output
        expected.content = execution.content
        if (execution.metadata !== undefined) expected.metadata = execution.metadata
      } else expected.error = execution.error?.message
      expected.assertions = assertions
      expected.tree = tree
      const normalized = JSON.stringify({ ...spec, fixtures: spec.fixtures ?? [], call, expected })
        .replaceAll(outside, "{{outside}}")
        .replaceAll(root, "{{root}}")
      exported.push(JSON.parse(normalized))
      await fs.rm(root, { recursive: true, force: true })
      await fs.rm(outside, { recursive: true, force: true })
    }
  } finally {
    await fs.rm(base, { recursive: true, force: true })
  }
  console.log(`__TOOL_CORPUS__${JSON.stringify(exported)}`)
}

const writeSpecs = (): ToolCase[] => [
  { name: "creates a relative file through FileMutation once", call: { path: "src/new.txt", content: "created" } },
  {
    name: "overwrites a relative existing file and reports that it wrote the file",
    fixtures: [{ path: "{{root}}/existing.txt", parts: [{ text: "before" }] }],
    call: { path: "existing.txt", content: "after" },
  },
  {
    name: "accepts an absolute file path inside the active Location",
    call: { path: "{{root}}/absolute.txt", content: "inside" },
  },
  {
    name: "writes an external symlink target with only its in-location permission",
    fixtures: [
      { path: "{{outside}}/external.txt", parts: [{ text: "before" }] },
      { path: "{{root}}/link.txt", kind: "symlink", target: "{{outside}}/external.txt" },
    ],
    call: { path: "link.txt", content: "after" },
  },
  {
    name: "approves an explicit external absolute path before edit",
    call: { path: "{{outside}}/external.txt", content: "external" },
  },
  {
    name: "saves external directory approval at the nearest project directory",
    fixtures: [
      { path: "{{outside}}/repo/.git", kind: "directory" },
      { path: "{{outside}}/repo/packages/app", kind: "directory" },
    ],
    call: { path: "{{outside}}/repo/packages/app/external.txt", content: "external" },
  },
  {
    name: "does not write when external_directory approval is denied",
    call: { path: "{{outside}}/denied.txt", content: "blocked" },
    permission: { deniedActions: ["external_directory"] },
  },
  {
    name: "does not write when edit approval is denied",
    call: { path: "denied.txt", content: "blocked" },
    permission: { deniedActions: ["edit"] },
  },
]

const editMissingLines =
  "import { a } from './a'\n\nexport function run() {\n  const value = a()\n  return value + 1\n}\n"

const editSpecs = (): ToolCase[] => [
  {
    name: "replaces relative exact text through FileMutation once",
    fixtures: [{ path: "{{root}}/hello.txt", parts: [{ text: "before\nrest\n" }] }],
    call: { path: "hello.txt", oldString: "before", newString: "after" },
  },
  {
    name: "accepts an absolute file path inside the active Location",
    fixtures: [{ path: "{{root}}/absolute.txt", parts: [{ text: "before" }] }],
    call: { path: "{{root}}/absolute.txt", oldString: "before", newString: "after" },
  },
  {
    name: "edits an external symlink target with only its in-location permission",
    fixtures: [
      { path: "{{outside}}/external.txt", parts: [{ text: "before" }] },
      { path: "{{root}}/link.txt", kind: "symlink", target: "{{outside}}/external.txt" },
    ],
    call: { path: "link.txt", oldString: "before", newString: "after" },
  },
  {
    name: "approves an explicit external absolute path before edit",
    fixtures: [{ path: "{{outside}}/external.txt", parts: [{ text: "before" }] }],
    call: { path: "{{outside}}/external.txt", oldString: "before", newString: "after" },
  },
  {
    name: "does not write when external_directory approval is denied",
    fixtures: [{ path: "{{outside}}/denied.txt", parts: [{ text: "before" }] }],
    call: { path: "{{outside}}/denied.txt", oldString: "before", newString: "after" },
    permission: { deniedActions: ["external_directory"] },
  },
  {
    name: "does not write when edit approval is denied",
    fixtures: [{ path: "{{outside}}/denied.txt", parts: [{ text: "before" }] }],
    call: { path: "{{outside}}/denied.txt", oldString: "before", newString: "after" },
    permission: { deniedActions: ["edit"] },
  },
  {
    name: "denied edit does not disclose whether oldString matches",
    fixtures: [{ path: "{{root}}/secret.txt", parts: [{ text: "secret content" }] }],
    call: { path: "secret.txt", oldString: "secret content", newString: "replacement" },
    permission: { deniedActions: ["edit"] },
  },
  {
    name: "denied edit does not disclose whether oldString is missing",
    fixtures: [{ path: "{{root}}/secret.txt", parts: [{ text: "secret content" }] }],
    call: { path: "secret.txt", oldString: "not present", newString: "replacement" },
    permission: { deniedActions: ["edit"] },
  },
  {
    name: "rejects an identical oldString and newString",
    fixtures: [{ path: "{{root}}/matches.txt", parts: [{ text: "same same" }] }],
    call: { path: "matches.txt", oldString: "same", newString: "same" },
  },
  {
    name: "rejects an empty oldString",
    fixtures: [{ path: "{{root}}/matches.txt", parts: [{ text: "same same" }] }],
    call: { path: "matches.txt", oldString: "", newString: "after" },
  },
  {
    name: "rejects a missing oldString",
    fixtures: [{ path: "{{root}}/matches.txt", parts: [{ text: "same same" }] }],
    call: { path: "matches.txt", oldString: "missing", newString: "after" },
  },
  {
    name: "rejects an ambiguous oldString",
    fixtures: [{ path: "{{root}}/matches.txt", parts: [{ text: "same same" }] }],
    call: { path: "matches.txt", oldString: "same", newString: "after" },
  },
  {
    name: "points a missed oldString at the closest lines",
    fixtures: [{ path: "{{root}}/miss.ts", parts: [{ text: editMissingLines }] }],
    call: {
      path: "miss.ts",
      oldString: "export function run() {\n\tconst value = a()\n\treturn value + 2\n}",
      newString: "x",
    },
  },
  {
    name: "points a missed oldString at newString already applied",
    fixtures: [{ path: "{{root}}/miss.ts", parts: [{ text: editMissingLines }] }],
    call: {
      path: "miss.ts",
      oldString: "const value = b()",
      newString: "  const value = a()\n  return value + 1",
    },
  },
  {
    name: "does not hint when no line matches",
    fixtures: [{ path: "{{root}}/miss.ts", parts: [{ text: editMissingLines }] }],
    call: { path: "miss.ts", oldString: "nothing like it", newString: "anything" },
  },
  {
    name: "does not hint past the matched span",
    fixtures: [{ path: "{{root}}/miss.ts", parts: [{ text: editMissingLines }] }],
    call: { path: "miss.ts", oldString: "import { a } from './a'\n\n\n\n\n\n", newString: "x" },
  },
  {
    name: "does not hint when reading the file is not allowed",
    fixtures: [{ path: "{{root}}/miss.ts", parts: [{ text: editMissingLines }] }],
    call: {
      path: "miss.ts",
      oldString: "export function run() {\n\tconst value = a()\n\treturn value + 2\n}",
      newString: "x",
    },
    permission: { readAllowed: false },
  },
  {
    name: "returns a missing file error",
    call: { path: "missing.ts", oldString: "before", newString: "after" },
  },
  {
    name: "returns a directory error",
    fixtures: [{ path: "{{root}}/src", kind: "directory" }],
    call: { path: "src", oldString: "before", newString: "after" },
  },
  {
    name: "replaces every exact occurrence when replaceAll is true",
    fixtures: [{ path: "{{root}}/all.txt", parts: [{ text: "same same same" }] }],
    call: { path: "all.txt", oldString: "same", newString: "after", replaceAll: true },
  },
  {
    name: "normalizes Unicode typography only after exact matching fails",
    fixtures: [
      {
        path: "{{root}}/unicode.txt",
        parts: [{ text: "exact - match\ncurly “quotes”\nminus − one\nspace\u00A0here\nexact − match\n" }],
      },
    ],
    call: {
      path: "unicode.txt",
      oldString: 'curly "quotes"\nminus - one\nspace here',
      newString: "normalized",
    },
  },
  {
    name: "ignores trailing whitespace while preserving untouched lines",
    fixtures: [{ path: "{{root}}/whitespace.txt", parts: [{ text: "before  \nmatch  \nnext\t\nafter  \n" }] }],
    call: { path: "whitespace.txt", oldString: "match\nnext", newString: "changed" },
  },
  {
    name: "uses non-overlapping trailing-whitespace matches",
    fixtures: [{ path: "{{root}}/overlap.txt", parts: [{ text: "a  \na  \na  \n" }] }],
    call: { path: "overlap.txt", oldString: "a\na", newString: "x", replaceAll: true },
  },
  {
    name: "preserves CRLF during a trailing-whitespace match",
    fixtures: [{ path: "{{root}}/windows.txt", parts: [{ text: "a  \r\nb\t\r\n" }] }],
    call: { path: "windows.txt", oldString: "a\nb", newString: "x" },
  },
  {
    name: "refuses to rewrite a file that is not valid UTF-8",
    fixtures: [
      {
        path: "{{root}}/Invoice.java",
        kind: "binary",
        base64: Buffer.concat([
          Buffer.from("// Jos\xe9\n", "latin1"),
          Buffer.from("int MAX = 10;\n"),
        ]).toString("base64"),
      },
    ],
    call: { path: "Invoice.java", oldString: "MAX = 10", newString: "MAX = 25" },
  },
  {
    name: "matches across mixed line endings and keeps each region's endings",
    fixtures: [{ path: "{{root}}/mostly-lf.txt", parts: [{ text: "head\r\nAAA OLD BBB\ntail\n" }] }],
    call: { path: "mostly-lf.txt", oldString: "AAA OLD BBB", newString: "AAA NEW\nLINE2" },
  },
  {
    name: "keeps endings while spanning lines in a mostly LF file",
    fixtures: [{ path: "{{root}}/mostly-lf.txt", parts: [{ text: "head\r\nAAA NEW\nLINE2\ntail\n" }] }],
    call: { path: "mostly-lf.txt", oldString: "LINE2\ntail", newString: "LINE2\nTAIL" },
  },
  {
    name: "keeps endings when a match crosses a mixed-ending region",
    fixtures: [{ path: "{{root}}/mostly-crlf.txt", parts: [{ text: "one\r\ntwo\r\nthree\nfour\r\n" }] }],
    call: { path: "mostly-crlf.txt", oldString: "o\nthree\nfo", newString: "O\nTHREE\nFO" },
  },
  {
    name: "keeps endings for a single-line match in a mostly CRLF file",
    fixtures: [{ path: "{{root}}/mostly-crlf.txt", parts: [{ text: "one\r\ntwo\r\nthree\nfour\r\n" }] }],
    call: { path: "mostly-crlf.txt", oldString: "one", newString: "ONE\nUNO" },
  },
  {
    name: "keeps the endings of unchanged and in-place lines inside a multi-line match",
    fixtures: [
      {
        path: "{{root}}/store.cs",
        parts: [
          {
            text: "All(item.Sku);\r\n\r\nFind(sku);\r\n\r\nvoid Add()\n{\n  _items[item.Sku] = item;\n}\r\n",
          },
        ],
      },
    ],
    call: {
      path: "store.cs",
      oldString: "All(item.Sku);\n\nFind(sku);\n\nvoid Add()\n{\n  _items[item.Sku] = item;\n}",
      newString:
        "All(item.Code);\n\nFind(code);\n\nvoid Add()\n{\n  _items[item.Code] = item;\n}\n\nbool Remove(string code)\n{\n}",
    },
  },
  {
    name: "strips line endings when oldString and newString differ only in \\r",
    fixtures: [{ path: "{{root}}/strip.sh", parts: [{ text: "a\r\nb\r\n" }] }],
    call: { path: "strip.sh", oldString: "\r", newString: "", replaceAll: true },
  },
  {
    name: "converts every line ending to LF",
    fixtures: [{ path: "{{root}}/whole.sh", parts: [{ text: "x\r\ny\r\n" }] }],
    call: { path: "whole.sh", oldString: "x\r\ny\r\n", newString: "x\ny\n" },
  },
  {
    name: "adds CR line endings",
    fixtures: [{ path: "{{root}}/add.bat", parts: [{ text: "p\nq\n" }] }],
    call: { path: "add.bat", oldString: "p\nq", newString: "p\r\nq" },
  },
  {
    name: "rejects an endings-only edit that is already applied",
    fixtures: [{ path: "{{root}}/add.bat", parts: [{ text: "p\r\nq\n" }] }],
    call: { path: "add.bat", oldString: "p\nq", newString: "p\r\nq" },
  },
  {
    name: "fails instead of reporting success when the edit leaves the file unchanged",
    fixtures: [{ path: "{{root}}/quote.txt", parts: [{ text: "it’s here\n" }] }],
    call: { path: "quote.txt", oldString: "it's here", newString: "it’s here" },
  },
]

const patchSpecs = (): ToolCase[] => [
  {
    name: "applies add, update, and delete hunks sequentially",
    fixtures: [
      { path: "{{root}}/update.txt", parts: [{ text: "before\n" }] },
      { path: "{{root}}/remove.txt", parts: [{ text: "remove\n" }] },
    ],
    call: {
      patchText:
        "*** Begin Patch\n*** Add File: nested/new.txt\n+created\n*** Update File: update.txt\n@@\n-before\n+after\n*** Delete File: remove.txt\n*** End Patch",
    },
  },
  {
    name: "replaces a file with a directory containing an added file",
    fixtures: [{ path: "{{root}}/parent", parts: [{ text: "before\n" }] }],
    call: {
      patchText: "*** Begin Patch\n*** Delete File: parent\n*** Add File: parent/child.txt\n+after\n*** End Patch",
    },
  },
  {
    name: "counts deleted lines with and without a trailing newline",
    fixtures: [
      { path: "{{root}}/trailing.txt", parts: [{ text: "remove\n" }] },
      { path: "{{root}}/unterminated.txt", parts: [{ text: "remove" }] },
    ],
    call: { patchText: "*** Begin Patch\n*** Delete File: trailing.txt\n*** Delete File: unterminated.txt\n*** End Patch" },
  },
  {
    name: "moves and updates a file",
    fixtures: [{ path: "{{root}}/old.txt", parts: [{ text: "before\n" }] }],
    call: {
      patchText:
        "*** Begin Patch\n*** Add File: created.txt\n+created\n*** Update File: old.txt\n*** Move to: moved.txt\n@@\n-before\n+after\n*** End Patch",
    },
  },
  {
    name: "moves a file over an existing destination",
    fixtures: [
      { path: "{{root}}/old.txt", parts: [{ text: "before\n" }] },
      { path: "{{root}}/nested/moved.txt", parts: [{ text: "existing\n" }] },
    ],
    call: {
      patchText: "*** Begin Patch\n*** Update File: old.txt\n*** Move to: nested/moved.txt\n@@\n-before\n+after\n*** End Patch",
    },
  },
  {
    name: "moves a file without changing its contents",
    fixtures: [{ path: "{{root}}/old.txt", parts: [{ text: "same\n" }] }],
    call: { patchText: "*** Begin Patch\n*** Update File: old.txt\n*** Move to: moved.txt\n@@\n same\n*** End Patch" },
  },
  {
    name: "refuses to rewrite a file that is not valid UTF-8 and applies none of the patch",
    fixtures: [
      { path: "{{root}}/a.txt", parts: [{ text: "before\n" }] },
      { path: "{{root}}/Invoice.java", kind: "binary", base64: latin1Java() },
    ],
    call: {
      patchText:
        "*** Begin Patch\n*** Update File: a.txt\n@@\n-before\n+after\n*** Update File: Invoice.java\n@@\n-int MAX = 10;\n+int MAX = 25;\n*** End Patch",
    },
  },
  {
    name: "deletes a file that is not valid UTF-8 without rewriting its bytes",
    fixtures: [{ path: "{{root}}/Old.java", kind: "binary", base64: latin1Java() }],
    call: { patchText: "*** Begin Patch\n*** Delete File: Old.java\n*** End Patch" },
  },
  {
    name: "renames a file when the move only changes its case",
    fixtures: [{ path: "{{root}}/same.txt", parts: [{ text: "before\n" }] }],
    call: { patchText: "*** Begin Patch\n*** Update File: same.txt\n*** Move to: Same.txt\n@@\n-before\n+after\n*** End Patch" },
  },
  {
    name: "moves into a directory whose name differs only in case",
    fixtures: [{ path: "{{root}}/Foo/a.txt", parts: [{ text: "before\n" }] }],
    call: {
      patchText: "*** Begin Patch\n*** Update File: Foo/a.txt\n*** Move to: foo/a.txt\n@@\n-before\n+after\n*** End Patch",
    },
  },
  {
    name: "moves a symlink without deleting its target",
    fixtures: [
      { path: "{{root}}/target.txt", parts: [{ text: "before\n" }] },
      { path: "{{root}}/link.txt", kind: "symlink", target: "{{root}}/target.txt" },
    ],
    call: { patchText: "*** Begin Patch\n*** Update File: link.txt\n*** Move to: moved.txt\n@@\n-before\n+after\n*** End Patch" },
  },
  {
    name: "includes move file info in metadata",
    fixtures: [{ path: "{{root}}/old/name.txt", parts: [{ text: "old content\n" }] }],
    call: {
      patchText:
        "*** Begin Patch\n*** Update File: old/name.txt\n*** Move to: renamed/dir/name.txt\n@@\n-old content\n+new content\n*** End Patch",
    },
  },
  {
    name: "uses Location-relative resources for move targets in a nested Location",
    location: { directory: "{{root}}/nested/location", projectDirectory: "{{root}}" },
    fixtures: [{ path: "{{root}}/nested/location/old.txt", parts: [{ text: "before\n" }] }],
    call: { patchText: "*** Begin Patch\n*** Update File: old.txt\n*** Move to: moved.txt\n@@\n-before\n+after\n*** End Patch" },
  },
  {
    name: "inserts lines with an insert-only hunk",
    fixtures: [{ path: "{{root}}/insert-only.txt", parts: [{ text: "alpha\nomega\n" }] }],
    call: { patchText: "*** Begin Patch\n*** Update File: insert-only.txt\n@@\n alpha\n+beta\n omega\n*** End Patch" },
  },
  {
    name: "rejects deleting a directory",
    fixtures: [{ path: "{{root}}/dir", kind: "directory" }],
    call: { patchText: "*** Begin Patch\n*** Delete File: dir\n*** End Patch" },
  },
  {
    name: "rejects a missing second chunk context",
    fixtures: [{ path: "{{root}}/two-chunks.txt", parts: [{ text: "a\nb\nc\nd\n" }] }],
    call: { patchText: "*** Begin Patch\n*** Update File: two-chunks.txt\n@@\n-b\n+B\n\n-d\n+D\n*** End Patch" },
  },
  { name: "requires patchText", call: { patchText: "" } },
  { name: "rejects invalid patch format", call: { patchText: "invalid patch" } },
  {
    name: "rejects a patch without an end marker",
    call: { patchText: "*** Begin Patch\n*** Add File: foo\n+hello" },
  },
  {
    name: "rejects an empty patch",
    call: { patchText: "*** Begin Patch\n*** End Patch" },
  },
  {
    name: "rejects an empty patch with whitespace",
    call: { patchText: " *** Begin Patch \n *** End Patch " },
  },
  {
    name: "rejects a heredoc-wrapped empty patch",
    call: { patchText: "<<EOF\n*** Begin Patch\n*** End Patch\nEOF" },
  },
  {
    name: "rejects an empty patch with an environment preamble",
    call: { patchText: "*** Begin Patch\n*** Environment ID: remote\n*** End Patch" },
  },
  {
    name: "rejects an invalid hunk header",
    call: { patchText: "*** Begin Patch\n*** Frobnicate File: foo\n*** End Patch" },
  },
  {
    name: "applies successive update operations to one file",
    fixtures: [{ path: "{{root}}/successive.txt", parts: [{ text: "a\nb\n" }] }],
    call: {
      patchText:
        "*** Begin Patch\n*** Update File: successive.txt\n@@\n-a\n+A\n*** Update File: successive.txt\n@@\n-b\n+B\n*** End Patch",
    },
  },
  {
    name: "preserves files across self-moves and multi-hunk moves in one patch",
    fixtures: [
      { path: "{{root}}/same.txt", parts: [{ text: "before\n" }] },
      { path: "{{root}}/old.txt", parts: [{ text: "a\nb\nc\n" }] },
    ],
    call: {
      patchText: [
        "*** Begin Patch",
        "*** Update File: same.txt",
        "*** Move to: ./same.txt",
        "@@",
        "-before",
        "+after",
        "*** Update File: old.txt",
        "@@",
        "-a",
        "+A",
        "*** Update File: old.txt",
        "*** Move to: moved.txt",
        "@@",
        "-b",
        "+B",
        "*** Update File: moved.txt",
        "@@",
        "-c",
        "+C",
        "*** End Patch",
      ].join("\n"),
    },
  },
  {
    name: "rejects an update with missing context",
    fixtures: [{ path: "{{root}}/unchanged.txt", parts: [{ text: "line1\nline2\n" }] }],
    call: {
      patchText: "*** Begin Patch\n*** Update File: unchanged.txt\n@@\n-missing\n+changed\n*** End Patch",
    },
  },
  {
    name: "rejects an update when the target file is missing",
    call: { patchText: "*** Begin Patch\n*** Update File: missing.txt\n@@\n-old\n+new\n*** End Patch" },
  },
  {
    name: "identifies a directory used as an update target",
    fixtures: [{ path: "{{root}}/nested", kind: "directory" }],
    call: { patchText: "*** Begin Patch\n*** Update File: nested\n@@\n-old\n+new\n*** End Patch" },
  },
  { name: "identifies a missing delete target", call: { patchText: "*** Begin Patch\n*** Delete File: missing.txt\n*** End Patch" } },
  {
    name: "approves an external directory before reading and requests edit permission afterward",
    fixtures: [
      { path: "{{outside}}/repository/.git", kind: "directory" },
      { path: "{{outside}}/repository/nested/external.txt", parts: [{ text: "before\n" }] },
    ],
    call: {
      patchText:
        "*** Begin Patch\n*** Update File: {{outside}}/repository/nested/external.txt\n@@\n-before\n+after\n*** End Patch",
    },
  },
  {
    name: "does not inspect an external file when external permission is denied",
    fixtures: [{ path: "{{outside}}/external.txt", parts: [{ text: "before\n" }] }],
    call: { patchText: "*** Begin Patch\n*** Update File: {{outside}}/external.txt\n@@\n-before\n+after\n*** End Patch" },
    permission: { deniedActions: ["external_directory"] },
  },
  {
    name: "preserves edit permission rejection",
    fixtures: [{ path: "{{root}}/target.txt", parts: [{ text: "before\n" }] }],
    call: { patchText: "*** Begin Patch\n*** Update File: target.txt\n@@\n-before\n+after\n*** End Patch" },
    permission: { deniedActions: ["edit"] },
  },
  {
    name: "treats a sibling path inside the project worktree as internal",
    location: { directory: "{{root}}/active", projectDirectory: "{{root}}" },
    fixtures: [
      { path: "{{root}}/active", kind: "directory" },
      { path: "{{root}}/sibling.txt", parts: [{ text: "before\n" }] },
    ],
    call: { patchText: "*** Begin Patch\n*** Update File: ../sibling.txt\n@@\n-before\n+after\n*** End Patch" },
  },
  {
    name: "follows an internal symlink to an external file without external permission",
    fixtures: [
      { path: "{{outside}}/external.txt", parts: [{ text: "before\n" }] },
      { path: "{{root}}/link.txt", kind: "symlink", target: "{{outside}}/external.txt" },
    ],
    call: { patchText: "*** Begin Patch\n*** Update File: link.txt\n@@\n-before\n+after\n*** End Patch" },
  },
  {
    name: "uses canonical external permissions and resources for a move destination",
    fixtures: [{ path: "{{root}}/source.txt", parts: [{ text: "before\n" }] }],
    call: {
      patchText:
        "*** Begin Patch\n*** Update File: source.txt\n*** Move to: {{outside}}/moved.txt\n@@\n-before\n+after\n*** End Patch",
    },
  },
  {
    name: "approves each external file under the same parent",
    fixtures: [
      { path: "{{outside}}/first.txt", parts: [{ text: "before\n" }] },
      { path: "{{outside}}/second.txt", parts: [{ text: "before\n" }] },
    ],
    call: {
      patchText:
        "*** Begin Patch\n*** Update File: {{outside}}/first.txt\n@@\n-before\n+after\n*** Update File: {{outside}}/second.txt\n@@\n-before\n+after\n*** End Patch",
    },
  },
  {
    name: "rejects invalid later update before applying an earlier add",
    call: {
      patchText:
        "*** Begin Patch\n*** Add File: created.txt\n+created\n*** Update File: missing.txt\n@@\n-before\n+after\n*** End Patch",
    },
  },
  {
    name: "adds files by overwriting existing targets",
    fixtures: [{ path: "{{root}}/existing.txt", parts: [{ text: "sentinel\n" }] }],
    call: { patchText: "*** Begin Patch\n*** Add File: existing.txt\n+replacement\n*** End Patch" },
  },
]

const latin1Java = () =>
  Buffer.concat([Buffer.from("// Jos\xe9\n", "latin1"), Buffer.from("int MAX = 10;\n")]).toString("base64")

function exportPatchParse() {
  const cases: Array<Record<string, unknown>> = []
  const parse = (name: string, input: string) => {
    const result = Patch.parse(input)
    if (Result.isFailure(result)) {
      cases.push({ name, operation: "parse", input, error: result.failure.message })
      return
    }
    cases.push({ name, operation: "parse", input, expected: result.success })
  }
  const derive = (
    name: string,
    path: string,
    chunks: Array<{ oldLines: string[]; newLines: string[]; changeContext?: string; endOfFile?: boolean }>,
    original: string,
  ) => {
    try {
      cases.push({ name, operation: "derive", path, chunks, original, expected: Patch.derive(path, chunks, original) })
    } catch (failure) {
      cases.push({
        name,
        operation: "derive",
        path,
        chunks,
        original,
        error: failure instanceof Error ? failure.message : String(failure),
      })
    }
  }

  parse(
    "parses add, update, and delete hunks",
    "*** Begin Patch\n*** Add File: add.txt\n+added\n*** Update File: update.txt\n@@ section\n-old\n+new\n*** Delete File: delete.txt\n*** End Patch",
  )
  parse("parses an empty patch", "*** Begin Patch\n*** End Patch")
  parse(
    "ignores a Codex environment preamble",
    "*** Begin Patch\n*** Environment ID: remote\n*** Add File: file.txt\n+content\n*** End Patch",
  )
  parse(
    "parses an update followed by an add",
    "*** Begin Patch\n*** Update File: update.txt\n@@\n+line\n*** Add File: add.txt\n+content\n*** End Patch",
  )
  parse(
    "parses a file move",
    "*** Begin Patch\n*** Update File: old.txt\n*** Move to: new.txt\n@@\n-old\n+new\n*** End Patch",
  )
  parse("rejects a missing first boundary", "This is not a valid patch")
  parse("rejects a missing last boundary", "*** Begin Patch\n*** Add File: add.txt\n+added")
  parse("rejects extra text before the first boundary", "extra\n*** Begin Patch\n*** End Patch")
  parse("rejects extra text after the last boundary", "*** Begin Patch\n*** Add File: add.txt\n+added\n*** End Patch\nextra")
  parse("allows whitespace after the end marker", "*** Begin Patch\n*** Add File: add.txt\n+added\n*** End Patch\n \t\n")
  parse(
    "strips a heredoc wrapper",
    "cat <<'EOF'\n*** Begin Patch\n*** Add File: add.txt\n+added\n*** End Patch\nEOF",
  )
  parse("strips a heredoc wrapper without cat", "<<EOF\n*** Begin Patch\n*** Add File: add.txt\n+added\n*** End Patch\nEOF")
  const heredocPatch = "*** Begin Patch\n*** Add File: add.txt\n+added\n*** End Patch"
  parse("strips a single-quoted heredoc wrapper", `<<'EOF'\n${heredocPatch}\nEOF`)
  parse("strips a double-quoted heredoc wrapper", `<<\"EOF\"\n${heredocPatch}\nEOF`)
  parse("rejects a malformed heredoc quote", `<<\"EOF'\n${heredocPatch}\nEOF`)
  parse("rejects a missing heredoc end", "<<EOF\n*** Begin Patch\n*** Add File: add.txt\n+added\nEOF")
  parse(
    "parses a whitespace-padded hunk header",
    "*** Begin Patch\n  *** Update File: foo.txt\n@@\n-old\n+new\n*** End Patch",
  )
  parse(
    "parses leading and trailing whitespace around patch markers",
    " *** Begin Patch\n*** Update File: file.txt\n@@\n-one\n+two\n*** End Patch ",
  )
  parse(
    "parses whitespace on the inner sides of patch marker lines",
    "*** Begin Patch \n*** Update File: file.txt\n@@\n-one\n+two\n *** End Patch",
  )
  parse(
    "parses relative and absolute hunk paths",
    "*** Begin Patch\n*** Add File: relative.txt\n+content\n*** Delete File: /tmp/delete.txt\n*** Update File: /tmp/update.txt\n@@\n-old\n+new\n*** End Patch",
  )
  parse(
    "strips one carriage return from CRLF patch lines",
    "*** Begin Patch\r\n*** Update File: file.txt\r\n@@\r\n-old\r\n+new\r\n*** End Patch\r\n",
  )
  parse(
    "preserves an extra carriage return in CRLF patch lines",
    "*** Begin Patch\r\n*** Update File: file.txt\r\n@@\r\n-old\r\r\n+new\r\n*** End Patch\r\n",
  )
  parse(
    "preserves the end-of-file marker",
    "*** Begin Patch\n*** Update File: file.txt\n@@\n+quux\n*** End of File\n\n*** End Patch",
  )
  parse(
    "allows an end-of-file marker before an explicit chunk",
    "*** Begin Patch\n*** Update File: file.txt\n*** End of File\n@@\n-old\n+new\n*** End Patch",
  )
  parse(
    "allows an end-of-file marker before an implicit chunk",
    "*** Begin Patch\n*** Update File: file.txt\n*** End of File\n-old\n+new\n*** End Patch",
  )
  parse(
    "allows an end-of-file marker before an implicit chunk and move",
    "*** Begin Patch\n*** Update File: old.txt\n*** End of File\n*** Move to: new.txt\n@@\n-old\n+new\n*** End Patch",
  )
  parse(
    "parses an update without an explicit first chunk header",
    "*** Begin Patch\n*** Update File: file.txt\n import foo\n+bar\n*** End Patch",
  )
  parse(
    "keeps indented update markers as context lines",
    "*** Begin Patch\n*** Update File: a.txt\n@@\n-old a\n+new a\n *** Update File: b.txt\n@@\n-old b\n+new b\n*** End Patch",
  )
  parse(
    "keeps indented move and EOF markers as context lines",
    "*** Begin Patch\n*** Update File: file.txt\n@@\n before\n *** Move to: moved.txt\n *** End of File\n*** End Patch",
  )
  parse(
    "preserves update context indentation",
    "*** Begin Patch\n*** Update File: file.txt\n@@   section\n-old\n+new\n*** End Patch",
  )
  parse(
    "preserves bare empty update lines as context",
    "*** Begin Patch\n*** Update File: file.txt\n@@\n context before\n\n context after\n*** End Patch",
  )
  parse("rejects an invalid add line", "*** Begin Patch\n*** Add File: file.txt\nbad\n*** End Patch")
  parse("rejects an invalid delete line", "*** Begin Patch\n*** Delete File: file.txt\nbad\n*** End Patch")
  parse(
    "rejects an invalid hunk after a delete",
    "*** Begin Patch\n*** Delete File: file.txt\n*** Frobnicate File: next.txt\n*** End Patch",
  )
  parse("rejects an empty update hunk", "*** Begin Patch\n*** Update File: file.txt\n*** End Patch")
  parse(
    "rejects an empty update hunk after a move",
    "*** Begin Patch\n*** Update File: old.txt\n*** Move to: new.txt\n*** Delete File: other.txt\n*** End Patch",
  )
  parse("rejects an empty update hunk after an EOF marker", "*** Begin Patch\n*** Update File: file.txt\n*** End of File\n*** End Patch")
  parse("rejects an empty update chunk", "*** Begin Patch\n*** Update File: file.txt\n@@\n*** End Patch")
  parse(
    "rejects an empty update chunk before an EOF marker",
    "*** Begin Patch\n*** Update File: file.txt\n@@\n*** End of File\n*** End Patch",
  )
  parse("rejects a repeated context marker", "*** Begin Patch\n*** Update File: file.txt\n@@\n@@\n-old\n+new\n*** End Patch")
  parse(
    "rejects a boundary inside an update chunk",
    "*** Begin Patch\n*** Update File: file.txt\n@@\n*** Update File: other.txt\n@@\n-old\n+new\n*** End Patch",
  )
  parse("rejects a bad update line", "*** Begin Patch\n*** Update File: file.txt\n@@\nbad\n*** End Patch")
  parse("rejects a bad line after a removed line", "*** Begin Patch\n*** Update File: file.txt\n@@\n-old\nbad\n*** End Patch")
  parse("rejects a bad context marker", "*** Begin Patch\n*** Update File: file.txt\n@@foo\n*** End Patch")
  parse(
    "rejects a bad line after a removed line and invalid header",
    "*** Begin Patch\n*** Update File: file.txt\n@@\n-old\n*** Frobnicate File: foo\n*** End Patch",
  )
  parse("rejects an invalid hunk header", "*** Begin Patch\n*** Frobnicate File: foo\n*** End Patch")
  parse("rejects a pathless add header", "*** Begin Patch\n*** Add File:\n*** End Patch")
  for (const header of ["*** Add File: ", "*** Delete File: ", "*** Update File: "])
    parse(`rejects a blank path in '${header.trim()}'`, `*** Begin Patch\n${header}\n*** End Patch`)
  parse(
    "rejects an empty move destination",
    "*** Begin Patch\n*** Update File: old.txt\n*** Move to: \n@@\n-old\n+new\n*** End Patch",
  )

  derive("derives fuzzy line updates while preserving BOM", "update.txt", [{ oldLines: ["  old   "], newLines: ["new"] }], "\uFEFFold\n")
  derive(
    "derives multiple update chunks",
    "update.txt",
    [
      { oldLines: ["line 2"], newLines: ["LINE 2"] },
      { oldLines: ["line 4"], newLines: ["LINE 4"] },
    ],
    "line 1\nline 2\nline 3\nline 4\n",
  )
  derive(
    "appends a pure-addition chunk to a nonempty file",
    "update.txt",
    [{ oldLines: [], newLines: ["added 1", "added 2"] }],
    "line 1\nline 2\n",
  )
  for (const original of ["", "original\n"])
    derive(`preserves equal-offset insertion order for ${JSON.stringify(original)}`, "update.txt", [
      { oldLines: [], newLines: ["first"] },
      { oldLines: [], newLines: ["second", "third"] },
    ], original)
  derive(
    "applies a pure-addition chunk after an earlier replacement",
    "update.txt",
    [
      { oldLines: [], newLines: ["after-context", "second-line"] },
      { oldLines: ["line1", "line2", "line3"], newLines: ["line1", "line2-replacement"] },
    ],
    "line1\nline2\nline3\n",
  )
  derive(
    "applies a deletion-only update chunk",
    "update.txt",
    [{ oldLines: ["line1", "line2", "line3"], newLines: ["line1", "line3"] }],
    "line1\nline2\nline3\n",
  )
  derive("updates an empty file and adds a trailing newline", "empty.txt", [{ oldLines: [], newLines: ["First line"] }], "")
  derive("adds a trailing newline to an unterminated file", "no-newline.txt", [{ oldLines: ["old"], newLines: ["new"] }], "old")
  derive(
    "disambiguates updates with change context",
    "update.txt",
    [{ oldLines: ["x=10"], newLines: ["x=11"], changeContext: "fn b" }],
    "fn a\nx=10\nfn b\nx=10\n",
  )
  derive("matches leading whitespace differences", "leading.txt", [{ oldLines: ["line"], newLines: ["next"] }], "  line\n")
  derive("matches trailing whitespace differences", "trailing.txt", [{ oldLines: ["line"], newLines: ["next"] }], "line  \n")
  derive(
    "matches Unicode punctuation differences",
    "unicode.txt",
    [{ oldLines: ['He said "hello"'], newLines: ['He said "hi"'] }],
    "He said “hello”\n",
  )
  derive(
    "matches Unicode minus signs",
    "minus.txt",
    [{ oldLines: ["value - 1"], newLines: ["value - 2"] }],
    "value − 1\n",
  )
  for (const space of [
    "\u00A0",
    "\u2002",
    "\u2003",
    "\u2004",
    "\u2005",
    "\u2006",
    "\u2007",
    "\u2008",
    "\u2009",
    "\u200A",
    "\u202F",
    "\u205F",
    "\u3000",
  ])
    derive(
      `matches the Unicode space U+${space.charCodeAt(0).toString(16).toUpperCase()}`,
      "spaces.txt",
      [{ oldLines: ["hello world"], newLines: ["hello there"] }],
      `hello${space}world\n`,
    )
  derive(
    "does not normalize ellipses",
    "ellipsis.txt",
    [{ oldLines: ["wait..."], newLines: ["done"] }],
    "wait…\n",
  )
  derive(
    "prefers a later exact match over an earlier normalized match",
    "quotes.txt",
    [{ oldLines: ['He said "hello"'], newLines: ['He said "goodbye"'] }],
    'He said “hello”\nmiddle\nHe said "hello"\n',
  )
  derive(
    "matches EOF-anchored chunks from the end",
    "update.txt",
    [{ oldLines: ["marker", "end"], newLines: ["marker changed", "end"], endOfFile: true }],
    "marker\nmiddle\nmarker\nend\n",
  )
  derive(
    "does not fall back to a non-EOF match",
    "update.txt",
    [{ oldLines: ["marker", "end"], newLines: ["changed", "end"], endOfFile: true }],
    "marker\nend\nmiddle\n",
  )
  derive("identifies a missing blank line", "update.txt", [{ oldLines: [""], newLines: ["added"] }], "content\n")

  console.log(`__TOOL_CORPUS__${JSON.stringify(cases)}`)
}

if (area === "definitions") await exportDefinitions()
else if (area === "snapshot") await exportSnapshot()
else if (area === "validation") await exportValidation()
else if (area === "truncate") await exportTruncate()
else if (area === "read_filesystem") await exportReadFilesystem()
else if (area === "read") await exportRead()
else if (area === "write") await exportToolCases("write", writeSpecs())
else if (area === "edit") await exportToolCases("edit", editSpecs())
else if (area === "patch_parse") exportPatchParse()
else if (area === "patch") await exportToolCases("patch", patchSpecs())
else {
  console.error(`corpus: unknown area ${area ?? ""}`)
  process.exit(1)
}
