import { pathToFileURL } from "node:url"
import { upstreamCoreTree } from "../upstream"

export type Transaction = {
  run: (statement: unknown) => unknown
  get: (statement: unknown) => unknown
  all: (statement: unknown) => unknown
}

export type Migration = {
  id: string
  foreignKeys?: boolean
  up: (tx: Transaction) => unknown
}

export async function loadMigrations() {
  const tree = await upstreamCoreTree()
  const effect = await import(pathToFileURL(Bun.resolveSync("effect", `${tree}/packages/core/src`)).href)
  const global = await import(pathToFileURL(`${tree}/packages/util/src/global.ts`).href)
  const migration = await import(pathToFileURL(`${tree}/packages/core/src/database/migration.gen.ts`).href)
  const schema = await import(pathToFileURL(`${tree}/packages/core/src/database/schema.gen.ts`).href)
  return {
    Effect: effect.Effect,
    Global: global.Global,
    migrations: migration.migrations as ReadonlyArray<Migration>,
    schema: schema.default as Migration,
  }
}
