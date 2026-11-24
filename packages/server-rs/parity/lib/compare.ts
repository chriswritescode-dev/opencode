export type Difference = { path: string; reference: unknown; candidate: unknown }

export const PARTS = ["steps", "events", "database", "files", "logs", "model"] as const

export type Part = (typeof PARTS)[number]

const join = (path: string, key: string) => (path.length === 0 ? key : `${path}/${key}`)

const isObject = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value)

const walk = (path: string, reference: unknown, candidate: unknown, differences: Difference[]) => {
  if (Array.isArray(reference) && Array.isArray(candidate)) {
    const length = Math.max(reference.length, candidate.length)
    for (let index = 0; index < length; index += 1) {
      const elementPath = join(path, String(index))
      if (index >= reference.length)
        differences.push({ path: elementPath, reference: undefined, candidate: candidate[index] })
      else if (index >= candidate.length)
        differences.push({ path: elementPath, reference: reference[index], candidate: undefined })
      else walk(elementPath, reference[index], candidate[index], differences)
    }
    return
  }
  if (isObject(reference) && isObject(candidate)) {
    for (const key of Object.keys(reference)) {
      const keyPath = join(path, key)
      if (Object.hasOwn(candidate, key)) walk(keyPath, reference[key], candidate[key], differences)
      else differences.push({ path: keyPath, reference: reference[key], candidate: undefined })
    }
    for (const key of Object.keys(candidate)) {
      if (Object.hasOwn(reference, key)) continue
      differences.push({ path: join(path, key), reference: undefined, candidate: candidate[key] })
    }
    return
  }
  if (reference !== candidate) differences.push({ path, reference, candidate })
}

export function compareCaptures(reference: unknown, candidate: unknown): Difference[] {
  const differences: Difference[] = []
  walk("", reference, candidate, differences)
  return differences
}

export function selectCapture(
  capture: Record<string, unknown>,
  parts: Set<Part>,
  tables?: string[],
): Record<string, unknown> {
  const selected = Object.fromEntries(Object.entries(capture).filter(([part]) => parts.has(part as Part)))
  if (tables !== undefined && isObject(selected.database)) selected.database = selectDatabase(selected.database, tables)
  return selected
}

function selectDatabase(database: Record<string, unknown>, tables: string[]) {
  return Object.fromEntries(Object.entries(database).map(([file, entry]) => [file, selectDatabaseEntry(entry, tables)]))
}

function selectDatabaseEntry(entry: unknown, tables: string[]) {
  if (!isObject(entry)) return entry
  const schema = Array.isArray(entry.schema)
    ? entry.schema.filter((row) => isObject(row) && typeof row.tbl_name === "string" && tables.includes(row.tbl_name))
    : entry.schema
  const selected = isObject(entry.tables)
    ? Object.fromEntries(Object.entries(entry.tables).filter(([table]) => tables.includes(table)))
    : entry.tables
  return { schema, tables: selected }
}
