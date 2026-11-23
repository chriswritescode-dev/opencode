import fs from "node:fs/promises"
import path from "node:path"
import { Database } from "bun:sqlite"

export type DatabaseSchema = { type: string; name: string; tbl_name: string; sql: string | null }

export type DatabaseCapture = { schema: DatabaseSchema[]; tables: Record<string, Record<string, unknown>[]> }

const SQLITE_HEADER = "SQLite format 3\u0000"

export async function dumpDatabases(root: string): Promise<Record<string, DatabaseCapture>> {
  const captured = await Promise.all(
    (await sqliteFiles(root)).map(
      async (relative) => [relative, await readDatabase(path.join(root, relative))] as const,
    ),
  )
  return Object.fromEntries(captured)
}

type SearchState = "absent" | "unavailable" | "building" | "done" | "failed"

export async function waitForSearchSettled(root: string): Promise<void> {
  const file = path.join(root, "home/.local/share/opencode/opencode.db")
  const deadline = Date.now() + 30_000
  let absentSince: number | undefined
  while (Date.now() < deadline) {
    const state = await readSearchState(file)
    if (state === "done" || state === "failed") return
    if (state === "absent") {
      absentSince ??= Date.now()
      if (Date.now() - absentSince >= 2_000) return
    } else {
      absentSince = undefined
    }
    await Bun.sleep(25)
  }
}

async function readSearchState(file: string): Promise<SearchState> {
  if (!(await Bun.file(file).exists())) return "absent"
  try {
    const database = new Database(file, { readonly: true })
    try {
      const row = database.query("select value from kv where key = 'session.search'").get() as {
        value?: unknown
      } | null
      if (row === null || typeof row.value !== "string") return "absent"
      const parsed = JSON.parse(row.value) as { status?: unknown }
      if (parsed.status === "building" || parsed.status === "done" || parsed.status === "failed")
        return parsed.status
      return "unavailable"
    } finally {
      database.close()
    }
  } catch {
    return "unavailable"
  }
}

export async function listFiles(root: string): Promise<Record<string, string>> {
  const databases = new Set(await sqliteFiles(root))
  const listed = await Promise.all(
    (await walkFiles(root)).map(async (relative) => {
      if (databases.has(relative)) return undefined
      if (databases.has(relative.replace(/-(?:wal|shm|journal)$/, ""))) return undefined
      if (relative === "models.json") return undefined
      if (relative.startsWith("home/.local/share/opencode/log/")) return undefined
      const bytes = new Uint8Array(await Bun.file(path.join(root, relative)).arrayBuffer())
      return [relative, decodeText(bytes) ?? hash(bytes)] as const
    }),
  )
  return Object.fromEntries(listed.filter((entry) => entry !== undefined))
}

async function walkFiles(root: string, directory: string = root): Promise<string[]> {
  const entries = await fs.readdir(directory, { withFileTypes: true })
  const nested = await Promise.all(
    entries.map(async (entry) => {
      const full = path.join(directory, entry.name)
      if (entry.isDirectory()) return walkFiles(root, full)
      if (entry.isFile()) return [relativePath(root, full)]
      return []
    }),
  )
  return nested.flat()
}

function relativePath(root: string, full: string): string {
  return path.relative(root, full).split(path.sep).join("/")
}

async function sqliteFiles(root: string): Promise<string[]> {
  const checked = await Promise.all(
    (await walkFiles(root)).map(async (relative) =>
      (await isSqlite(path.join(root, relative))) ? relative : undefined,
    ),
  )
  return checked.filter((relative): relative is string => relative !== undefined)
}

async function isSqlite(file: string): Promise<boolean> {
  const header = await Bun.file(file).slice(0, 16).arrayBuffer()
  return new TextDecoder().decode(header) === SQLITE_HEADER
}

async function readDatabase(file: string): Promise<DatabaseCapture> {
  const database = new Database(file, { readonly: true })
  try {
    const schema = (database.query("select type, name, tbl_name, sql from sqlite_master").all() as DatabaseSchema[])
      .map((row) => ({ type: row.type, name: row.name, tbl_name: row.tbl_name, sql: row.sql }))
      .sort(compareSchema)
    const tables = Object.fromEntries(
      schema.filter((row) => row.type === "table").map((row) => [row.name, readTable(database, row.name)]),
    )
    return { schema, tables }
  } finally {
    database.close()
  }
}

function compareSchema(a: DatabaseSchema, b: DatabaseSchema): number {
  if (a.type !== b.type) return a.type < b.type ? -1 : 1
  if (a.name !== b.name) return a.name < b.name ? -1 : 1
  return 0
}

function readTable(database: Database, name: string): Record<string, unknown>[] {
  const quoted = name.replace(/"/g, '""')
  return (database.query(`select * from "${quoted}"`).all() as Record<string, unknown>[]).map(plainRow)
}

function plainRow(row: Record<string, unknown>): Record<string, unknown> {
  return Object.fromEntries(Object.entries(row).map(([key, value]) => [key, plainValue(value)]))
}

function plainValue(value: unknown): unknown {
  if (value instanceof Uint8Array) return `hex:${Buffer.from(value).toString("hex")}`
  if (typeof value === "bigint") return Number(value)
  return value
}

function decodeText(bytes: Uint8Array): string | undefined {
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes)
  } catch {
    return undefined
  }
}

function hash(bytes: Uint8Array): string {
  const hasher = new Bun.CryptoHasher("sha256")
  hasher.update(bytes)
  return `sha256:${hasher.digest("hex")}`
}
