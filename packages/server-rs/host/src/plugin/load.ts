import path from "node:path"
import { readFile, stat } from "node:fs/promises"
import { fileURLToPath, pathToFileURL } from "node:url"
import { Effect, Option, Schema } from "effect"
import type { Plugin } from "@opencode/plugin/effect/plugin"
import { Host } from "@opencode/plugin/host"
import { fromPromise } from "@opencode/plugin/promise/adapter"
import { createPluginSources } from "@opencode/plugin/source"
import { GuiBundle, type Source } from "@opencode/schema/plugin"
import { Hash } from "@opencode/util/hash"
import type { Context } from "../connection"
import type { JsonValue, LocationRef } from "../protocol.gen"
import { npm, type NpmPaths } from "./npm"

export type LoadOperation = {
  type: "add"
  target: string
  options: Record<string, JsonValue>
  mtime?: number
}

export type LoadParams = {
  operation: LoadOperation
  install: boolean
  paths: NpmPaths
}

export type Definition = {
  id: string
  effect: Plugin["effect"]
}

export type LoadResult =
  | { pending: true }
  | {
      handle: string
      id: string
      features: { server?: true; tui?: true; rpc?: true; gui?: true }
      gui?: { file: string; revision: string; options: Record<string, unknown> }
      revision: string
      source: Source
    }

type Entry = {
  sources: ReturnType<typeof createPluginSources>
  definitions: Map<string, Definition>
  ctx: Context
}

const registry = new Map<string, Entry>()

const GuiOptions = GuiBundle.fields.options

const Module = Schema.Struct({
  default: Schema.Union([
    Schema.Struct({
      id: Schema.String,
      effect: Schema.declare<Plugin["effect"]>((input): input is Plugin["effect"] => typeof input === "function"),
    }),
    Schema.Struct({
      id: Schema.String,
      setup: Schema.declare<Parameters<typeof fromPromise>[0]["setup"]>(
        (input): input is Parameters<typeof fromPromise>[0]["setup"] => typeof input === "function",
      ),
    }),
  ]),
})

export function locationKey(location: LocationRef) {
  return location.directory + "\u0000" + (location.workspaceID ?? "")
}

export function definition(handle: string) {
  const found = registry.get(handle.slice(0, handle.lastIndexOf("\u0000")))?.definitions.get(handle)
  if (found === undefined) throw new Error(`Unknown plugin handle: ${handle}`)
  return found
}

export function forget(location: LocationRef) {
  const key = locationKey(location)
  const entry = registry.get(key)
  if (entry === undefined) return
  entry.sources.dispose()
  entry.definitions.clear()
  registry.delete(key)
}

export function source(target: string, version?: string): Source {
  if (path.isAbsolute(target)) return { type: "local", path: target }
  return { type: "package", target, ...(version ? { version } : {}) }
}

export async function load(params: LoadParams, ctx: Context): Promise<LoadResult> {
  if (ctx.location === undefined) throw new Error("plugin location is required")
  const entry = entryFor(ctx)
  const operation = params.operation
  const target = operation.target
  const local = path.isAbsolute(target)
  const installed = local
    ? undefined
    : params.install === false
      ? await npm(params.paths).resolve(target)
      : await npm(params.paths).add(target)
  const entrypoints: Host.Entrypoints =
    local && (await stat(target)).isFile()
      ? { server: pathToFileURL(target).href }
      : Host.resolve(installed ?? { directory: target })
  const entrypoint = entrypoints.server
  const found = entrypoint || entrypoints.gui
  if (!local && params.install === false && !found) return { pending: true }
  if (!found) throw loadError(`Plugin entrypoint not found: ${target}`)
  const server = entrypoint
    ? await loadServer(entrypoint, local ? entry.sources : undefined, installed?.revision)
    : undefined
  const plugin: Definition = server?.plugin ?? {
    id: Host.packageID(installed ?? { directory: target }),
    effect: () => Effect.void,
  }
  const revision = JSON.stringify([operation, server ? server.version : installed?.revision])
  const guiFile = local && entrypoints.gui ? fileURLToPath(entrypoints.gui) : undefined
  if (guiFile !== undefined) await watch(entry.ctx, guiFile)
  const bundle = guiFile !== undefined ? Hash.sha256(await readFile(guiFile)) : undefined
  const guiOptions = entrypoints.gui ? decodeGuiOptions(operation.options) : {}
  const handle = locationKey(ctx.location) + "\u0000" + revision
  entry.definitions.set(handle, plugin)
  return {
    handle,
    id: plugin.id,
    features: {
      ...(entrypoint ? { server: true as const } : {}),
      ...(entrypoints.tui ? { tui: true as const } : {}),
      ...(entrypoint && entrypoints.rpc ? { rpc: true as const } : {}),
      ...(entrypoints.gui ? { gui: true as const } : {}),
    },
    ...(entrypoints.gui
      ? {
          gui: {
            file: fileURLToPath(entrypoints.gui),
            revision: Hash.sha256(JSON.stringify([revision, bundle])),
            options: guiOptions,
          },
        }
      : {}),
    revision,
    source: source(target, installed?.version),
  }
}

function entryFor(ctx: Context) {
  if (ctx.location === undefined) throw new Error("plugin location is required")
  const key = locationKey(ctx.location)
  const existing = registry.get(key)
  if (existing !== undefined) return existing
  const entry: Entry = {
    sources: createPluginSources((file) => watch(ctx, file)),
    definitions: new Map(),
    ctx,
  }
  registry.set(key, entry)
  return entry
}

async function watch(ctx: Context, file: string) {
  await ctx.call("plugin.watch", { file })
}

async function loadServer(
  entrypoint: string,
  sources: ReturnType<typeof createPluginSources> | undefined,
  revision: string | undefined,
): Promise<{ plugin: Definition; version: string | undefined }> {
  const loaded = sources
    ? await sources.read(entrypoint)
    : await Host.load(entrypoint).then((module) => ({ module, version: revision }))
  const decoded = Option.getOrUndefined(Schema.decodeUnknownOption(Module)(loaded.module))
  if (decoded === undefined)
    throw loadError("Plugin must export a default definition with an id and an effect or setup function.")
  const value = decoded.default
  return { plugin: "effect" in value ? value : fromPromise(value), version: loaded.version }
}

function decodeGuiOptions(options: Record<string, JsonValue>) {
  const decoded = Schema.decodeUnknownOption(GuiOptions)(options)
  if (Option.isNone(decoded)) throw loadError("Plugin options must be JSON to reach its ./gui-experimental")
  return decoded.value
}

function loadError(message: string) {
  const error = new Error(message)
  error.name = "PluginModule.LoadError"
  return error
}
