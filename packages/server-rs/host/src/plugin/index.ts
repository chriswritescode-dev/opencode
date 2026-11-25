import type { Handler, Context } from "../connection"
import type { JsonValue } from "../protocol.gen"
import { activate, close, deactivate, type ActivateParams } from "./activate"
import { load, type LoadParams } from "./load"
import { npm, type NpmPaths } from "./npm"
import { resolveLocal } from "./resolve"
import { installRuntime } from "./runtime"

installRuntime()

export const handlers: Record<string, Handler> = {
  "plugin.load": async (ctx, params) => {
    requireLocation(ctx)
    return { value: (await load(params as unknown as LoadParams, ctx)) as unknown as JsonValue }
  },
  "plugin.activate": async (ctx, params) => {
    const location = requireLocation(ctx)
    const list = async () => (await ctx.call("plugin.list", null)).value
    return {
      value: (await activate(params as unknown as ActivateParams, list, location)) as unknown as JsonValue,
    }
  },
  "plugin.deactivate": async (ctx, params) => {
    await deactivate(stringField(record(params).activation, "activation"), requireLocation(ctx))
    return { value: null }
  },
  "plugin.close": async (ctx) => {
    await close(requireLocation(ctx))
    return { value: null }
  },
  "npm.add": async (_ctx, params) => ({
    value: (await npm(npmPaths(params)).add(packageName(params))) as unknown as JsonValue,
  }),
  "npm.resolve": async (_ctx, params) => ({
    value: (await npm(npmPaths(params)).resolve(packageName(params))) as unknown as JsonValue,
  }),
  "npm.check": async (_ctx, params) => ({ value: await npm(npmPaths(params)).check(packageName(params)) }),
  "npm.update": async (_ctx, params) => ({
    value: (await npm(npmPaths(params)).update(packageName(params))) as unknown as JsonValue,
  }),
  "npm.follows": async (_ctx, params) => ({ value: await npm(npmPaths(params)).follows(packageName(params)) }),
  "npm.reject": async (_ctx, params) => {
    const version = record(params).version
    if (typeof version !== "string") throw new Error("npm.reject requires a version")
    return { value: await npm(npmPaths(params)).reject(packageName(params), version) }
  },
  "plugin.resolve": async (_ctx, params) => {
    const directory = stringField(record(params).directory, "directory")
    return { value: resolveLocal({ directory }) as unknown as JsonValue }
  },
}

function record(params: JsonValue) {
  if (typeof params !== "object" || params === null || Array.isArray(params)) {
    throw new Error("params must be an object")
  }
  return params
}

function stringField(value: JsonValue | undefined, name: string) {
  if (typeof value !== "string") throw new Error(`${name} must be a string`)
  return value
}

function npmPaths(params: JsonValue) {
  const value = record(params).paths
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error("paths must be an object")
  }
  return value as NpmPaths
}

function packageName(params: JsonValue) {
  return stringField(record(params).pkg, "pkg")
}

function requireLocation(ctx: Context) {
  if (ctx.location === undefined) throw new Error("plugin location is required")
  return ctx.location
}
