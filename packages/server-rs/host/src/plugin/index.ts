import type { Handler } from "../connection"
import type { JsonValue } from "../protocol.gen"
import { npm, type NpmPaths } from "./npm"
import { resolveLocal } from "./resolve"
import { installRuntime } from "./runtime"

installRuntime()

export const handlers: Record<string, Handler> = {
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
