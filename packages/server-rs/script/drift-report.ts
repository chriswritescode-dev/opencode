import { pin } from "./upstream"

const root = new URL("..", import.meta.url).pathname
const methods = new Set(["get", "put", "post", "delete", "options", "head", "patch", "trace"])

type TypeSpec = {
  enum?: ReadonlyArray<string>
  const?: string
  pattern?: string
}

type SchemaObject = {
  anyOf?: ReadonlyArray<{ $ref?: string }>
  properties?: { type?: TypeSpec }
}

type DocumentShape = {
  paths: Record<string, Record<string, unknown>>
  components: { schemas: Record<string, SchemaObject> }
}

type OperationEntry = { method: string; path: string; operation: unknown }
type MigrationChange = { kind: "added" | "removed"; path: string }

const args = process.argv.slice(2)
const fromIndex = args.indexOf("--from")
const fromArgument = fromIndex === -1 ? undefined : args[fromIndex + 1]
const target = args.find(
  (argument, index) => argument !== "--from" && index !== (fromIndex === -1 ? -1 : fromIndex + 1),
)

if (target === undefined) fail("usage: bun run script/drift-report.ts <commit> [--from <commit>]")
if (fromIndex !== -1 && fromArgument === undefined) fail("--from requires a commit")

const topResult = Bun.spawnSync(["git", "rev-parse", "--show-toplevel"], { cwd: root })
if (topResult.exitCode !== 0) fail(`git rev-parse failed: ${topResult.stderr.toString().trim()}`)
const top = topResult.stdout.toString().trim()

const from = fromArgument ?? (await pin())
const base = documentAt(from)
const head = documentAt(target)

const baseOperations = operationsOf(base)
const targetOperations = operationsOf(head)
const operationDelta = compare(baseOperations, targetOperations, (entry) => entry.operation)
const baseSchemas = base.components.schemas
const targetSchemas = head.components.schemas
const schemaDelta = compare(new Map(Object.entries(baseSchemas)), new Map(Object.entries(targetSchemas)))
const eventDelta = listDelta(eventTypes(baseSchemas, "V2Event"), eventTypes(targetSchemas, "V2Event"))
const sessionLogDelta = listDelta(
  eventTypes(baseSchemas, "SessionLogItem"),
  eventTypes(targetSchemas, "SessionLogItem"),
)
const migrationChanges = migrationsBetween(from, target)
const migrationsAdded = migrationChanges.filter((change) => change.kind === "added").map((change) => change.path)
const migrationsRemoved = migrationChanges.filter((change) => change.kind === "removed").map((change) => change.path)

const lines: Array<string> = ["# Upstream drift report", "", `- from: \`${from}\``, `- to: \`${target}\``]

section(
  "Operations",
  `${operationDelta.added.length} added, ${operationDelta.removed.length} removed, ${operationDelta.changed.length} changed`,
  [
    ["Added", operationDelta.added.map((id) => describeOperation(id, targetOperations.get(id)))],
    ["Removed", operationDelta.removed.map((id) => describeOperation(id, baseOperations.get(id)))],
    ["Changed", operationDelta.changed.map((id) => describeOperation(id, targetOperations.get(id)))],
  ],
)
section(
  "Component schemas",
  `${schemaDelta.added.length} added, ${schemaDelta.removed.length} removed, ${schemaDelta.changed.length} changed`,
  [
    ["Added", schemaDelta.added.map(code)],
    ["Removed", schemaDelta.removed.map(code)],
    ["Changed", schemaDelta.changed.map(code)],
  ],
)
section("Public events", `${eventDelta.added.length} added, ${eventDelta.removed.length} removed`, [
  ["Added", eventDelta.added.map(code)],
  ["Removed", eventDelta.removed.map(code)],
])
section(
  "Durable/session-log members",
  `${sessionLogDelta.added.length} added, ${sessionLogDelta.removed.length} removed`,
  [
    ["Added", sessionLogDelta.added.map(code)],
    ["Removed", sessionLogDelta.removed.map(code)],
  ],
)
section("Migrations", `${migrationsAdded.length} added, ${migrationsRemoved.length} removed`, [
  ["Added", migrationsAdded.map(code)],
  ["Removed", migrationsRemoved.map(code)],
])

console.log(lines.join("\n"))

function documentAt(commit: string): DocumentShape {
  const result = Bun.spawnSync(["git", "show", `${commit}:packages/protocol/openapi.json`], { cwd: top })
  if (result.exitCode !== 0) fail(`cannot read packages/protocol/openapi.json at ${commit}`)
  return parseDocument(result.stdout.toString(), commit)
}

function parseDocument(text: string, commit: string): DocumentShape {
  try {
    return JSON.parse(text)
  } catch {
    return fail(`cannot parse packages/protocol/openapi.json at ${commit}`)
  }
}

function operationsOf(document: DocumentShape) {
  return new Map(
    Object.entries(document.paths).flatMap(([path, item]) =>
      Object.entries(item).flatMap(([method, value]) =>
        methods.has(method) && isOperation(value) ? [operationEntry(method, path, value)] : [],
      ),
    ),
  )
}

function operationEntry(method: string, path: string, operation: { operationId: string }): [string, OperationEntry] {
  return [operation.operationId, { method: method.toUpperCase(), path, operation }]
}

function isOperation(value: unknown): value is { operationId: string } {
  return typeof value === "object" && value !== null && "operationId" in value && typeof value.operationId === "string"
}

function compare<T>(
  fromValues: Map<string, T>,
  toValues: Map<string, T>,
  value: (entry: T) => unknown = (entry) => entry,
) {
  return {
    added: [...toValues.keys()].filter((key) => !fromValues.has(key)),
    removed: [...fromValues.keys()].filter((key) => !toValues.has(key)),
    changed: [...fromValues.keys()].filter(
      (key) => toValues.has(key) && canonical(value(fromValues.get(key)!)) !== canonical(value(toValues.get(key)!)),
    ),
  }
}

function canonical(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`
  if (value === null || typeof value !== "object") return JSON.stringify(value) ?? "null"
  return `{${Object.keys(value)
    .sort()
    .map((key) => `${JSON.stringify(key)}:${canonical(Object.getOwnPropertyDescriptor(value, key)?.value)}`)
    .join(",")}}`
}

function listDelta(fromValues: ReadonlyArray<string>, toValues: ReadonlyArray<string>) {
  const fromSet = new Set(fromValues)
  const toSet = new Set(toValues)
  return {
    added: toValues.filter((value) => !fromSet.has(value)),
    removed: fromValues.filter((value) => !toSet.has(value)),
  }
}

function eventTypes(schemas: Record<string, SchemaObject>, name: string) {
  const schema = schemas[name]
  return schema === undefined ? [] : collectEventTypes(schemas, schema, new Set([name]))
}

function collectEventTypes(
  schemas: Record<string, SchemaObject>,
  schema: SchemaObject,
  seen: ReadonlySet<string>,
): Array<string> {
  if (schema.anyOf !== undefined)
    return schema.anyOf.flatMap((member) => {
      const name = member.$ref === undefined ? undefined : refName(member.$ref)
      if (name === undefined || seen.has(name)) return []
      const target = schemas[name]
      return target === undefined ? [] : collectEventTypes(schemas, target, new Set([...seen, name]))
    })
  const type = schema.properties?.type
  if (type === undefined || type.pattern !== undefined) return []
  const value = type.enum === undefined ? type.const : type.enum[0]
  return typeof value === "string" ? [value] : []
}

function refName(ref: string) {
  return ref.slice(ref.lastIndexOf("/") + 1)
}

function migrationsBetween(fromCommit: string, toCommit: string): Array<MigrationChange> {
  const result = Bun.spawnSync(
    ["git", "diff", "--name-status", fromCommit, toCommit, "--", "packages/core/src/database/migration/"],
    { cwd: top },
  )
  if (result.exitCode !== 0) fail(`git diff failed: ${result.stderr.toString().trim()}`)
  return result.stdout
    .toString()
    .split("\n")
    .filter((line) => line.length > 0)
    .flatMap((line) => migrationChange(line.split("\t")))
}

function migrationChange([status, first, second]: Array<string | undefined>): Array<MigrationChange> {
  if (status === undefined || first === undefined) return []
  if (status === "A") return [{ kind: "added", path: first }]
  if (status === "D") return [{ kind: "removed", path: first }]
  if (status.startsWith("R") && second !== undefined)
    return [
      { kind: "removed", path: first },
      { kind: "added", path: second },
    ]
  return []
}

function describeOperation(id: string, entry: OperationEntry | undefined) {
  return entry === undefined ? code(id) : `${code(id)} \`${entry.method} ${entry.path}\``
}

function code(value: string) {
  return `\`${value}\``
}

function section(title: string, summary: string, groups: ReadonlyArray<readonly [string, ReadonlyArray<string>]>) {
  lines.push("", `## ${title}`, "", summary)
  for (const [heading, items] of groups) {
    if (items.length === 0) continue
    lines.push("", `### ${heading}`, "", ...items.map((item) => `- ${item}`))
  }
}

function fail(message: string): never {
  console.error(`drift-report: ${message}`)
  process.exit(1)
}
