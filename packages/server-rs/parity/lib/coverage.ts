import fs from "node:fs/promises"
import path from "node:path"

export type Operation = { id: string; method: string; path: string }
export type Surface = { operations: Operation[]; websockets: Operation[]; events: string[] }
export type RequestRecord = { protocol: "http" | "ws"; method: string; path: string }
export type RunCoverage = { name: string; requests: RequestRecord[]; events: unknown[] }
export type CoverageItem = {
  id: string
  method?: string
  path?: string
  covered: boolean
  scenarios: string[]
  compared: boolean
}
export type Coverage = {
  operations: CoverageItem[]
  events: CoverageItem[]
  websockets: CoverageItem[]
  unmatched: string[]
}

type OpenApiDocument = { paths: Record<string, Record<string, unknown>> }
type Ranked = { operation: Operation; index: number }

const openApiPath = "packages/protocol/openapi.json"
const pinPath = "packages/server-rs/upstream-pin"
const httpMethods = new Set(["get", "post", "put", "patch", "delete"])
const wildcard = "*"
const parameter = /^\{[^{}]+\}$/

export async function loadSurface(repository: string): Promise<Surface> {
  const document = await readDocument(repository)
  const operations: Operation[] = []
  const websockets: Operation[] = []
  for (const [template, entry] of Object.entries(document.paths)) {
    for (const [method, value] of Object.entries(entry)) {
      if (!httpMethods.has(method) || !isRecord(value)) continue
      const id = value.operationId
      if (typeof id !== "string") continue
      const operation = { id, method: method.toUpperCase(), path: template }
      operations.push(operation)
      if (value["x-websocket"] === true) websockets.push(operation)
    }
  }
  return { operations, websockets, events: eventTypes(document) }
}

async function readDocument(repository: string): Promise<OpenApiDocument> {
  const pin = await readPin(repository)
  if (pin !== undefined) {
    const result = Bun.spawnSync(["git", "show", `${pin}:${openApiPath}`], {
      cwd: repository,
      stdout: "pipe",
      stderr: "ignore",
    })
    if (result.exitCode === 0) return JSON.parse(result.stdout.toString())
  }
  console.warn(`warning: cannot read ${openApiPath} at ${pin ?? "the pinned commit"}; using the checkout copy`)
  return JSON.parse(await Bun.file(path.join(repository, openApiPath)).text())
}

async function readPin(repository: string) {
  const file = Bun.file(path.join(repository, pinPath))
  if (!(await file.exists())) return undefined
  const pin = (await file.text()).trim()
  return pin.length === 0 ? undefined : pin
}

function eventTypes(document: OpenApiDocument) {
  const operation = document.paths["/api/event"]?.get
  const parameters = isRecord(operation) && Array.isArray(operation.parameters) ? operation.parameters : []
  const types = parameters.find((entry) => isRecord(entry) && entry.name === "types")
  const schema = isRecord(types) ? types.schema : undefined
  const events = [...new Set(isRecord(schema) ? collectEnums(schema) : [])]
  for (const extra of ["server.connected", "rpc.*"]) if (!events.includes(extra)) events.push(extra)
  return events
}

function collectEnums(schema: unknown): string[] {
  if (Array.isArray(schema)) return schema.flatMap(collectEnums)
  if (!isRecord(schema)) return []
  const own = Array.isArray(schema.enum)
    ? schema.enum.filter((value): value is string => typeof value === "string")
    : []
  return [
    ...own,
    ...collectEnums(schema.items),
    ...collectEnums(schema.anyOf),
    ...collectEnums(schema.oneOf),
    ...collectEnums(schema.allOf),
  ]
}

export function matchOperation(surface: Surface, method: string, urlPath: string): Operation | undefined {
  return findMatch(surface.operations, method, urlPath)
}

function findMatch(operations: Operation[], method: string, urlPath: string): Operation | undefined {
  const wanted = method.toUpperCase()
  const matches = operations
    .map((operation, index) => ({ operation, index }))
    .filter((entry) => entry.operation.method === wanted && templatePattern(entry.operation.path).test(urlPath))
    .sort(compareSpecificity)
  return matches.length === 0 ? undefined : matches[0].operation
}

function compareSpecificity(left: Ranked, right: Ranked) {
  return (
    wildcards(left.operation.path) - wildcards(right.operation.path) ||
    parameters(left.operation.path) - parameters(right.operation.path) ||
    left.index - right.index
  )
}

function wildcards(template: string) {
  return template.split("/").filter((segment) => segment === wildcard).length
}

function parameters(template: string) {
  return template.split("/").filter((segment) => parameter.test(segment)).length
}

function templatePattern(template: string) {
  const pattern = template
    .split("/")
    .map((segment) => {
      if (segment === wildcard) return ".*"
      if (parameter.test(segment)) return "[^/]+"
      return segment.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")
    })
    .join("/")
  return new RegExp(`^${pattern}$`)
}

export function coverageReport(
  surface: Surface,
  runs: RunCoverage[],
  compared: { operations: boolean; events: boolean; websockets: boolean } = {
    operations: true,
    events: true,
    websockets: true,
  },
): Coverage {
  const operations = surface.operations.map((operation) => coverageItem(operation, compared.operations))
  const events = surface.events.map((event) => ({
    id: event,
    covered: false,
    scenarios: [] as string[],
    compared: compared.events,
  }))
  const websockets = surface.websockets.map((operation) => coverageItem(operation, compared.websockets))
  const unmatched: string[] = []
  for (const run of runs) {
    for (const request of run.requests) {
      if (request.protocol === "http" && !compared.operations) continue
      if (request.protocol === "ws" && !compared.websockets) continue
      const targets = request.protocol === "http" ? surface.operations : surface.websockets
      const list = request.protocol === "http" ? operations : websockets
      const matched = findMatch(targets, request.method, request.path)
      if (matched === undefined) {
        addUnique(unmatched, `${request.method} ${request.path}`)
        continue
      }
      const item = list.find((entry) => entry.id === matched.id)
      if (item !== undefined) addUnique(item.scenarios, run.name)
    }
    if (!compared.events) continue
    for (const event of run.events) {
      if (!isRecord(event) || typeof event.type !== "string") continue
      const type = event.type
      const target = type.startsWith("rpc.") && surface.events.includes("rpc.*") ? "rpc.*" : type
      const item = events.find((entry) => entry.id === target)
      if (item !== undefined) addUnique(item.scenarios, run.name)
    }
  }
  for (const item of [...operations, ...events, ...websockets]) {
    item.scenarios.sort()
    item.covered = item.scenarios.length > 0
  }
  return { operations, events, websockets, unmatched }
}

function coverageItem(operation: Operation, compared: boolean): CoverageItem {
  return { id: operation.id, method: operation.method, path: operation.path, covered: false, scenarios: [], compared }
}

function addUnique(values: string[], value: string) {
  if (!values.includes(value)) values.push(value)
}

export function coverageSummary(coverage: Coverage) {
  return `coverage: operations ${count(coverage.operations)}, events ${count(coverage.events)}, websockets ${count(coverage.websockets)}`
}

function coverageTotals(coverage: Coverage) {
  return `operations: ${count(coverage.operations)}, events: ${count(coverage.events)}, websockets: ${count(coverage.websockets)}`
}

function count(items: CoverageItem[]) {
  if (items.some((item) => !item.compared)) return "not compared"
  return `${items.filter((item) => item.covered).length}/${items.length}`
}

export async function writeCoverage(outputRoot: string, coverage: Coverage) {
  await fs.mkdir(outputRoot, { recursive: true })
  await Promise.all([
    Bun.write(path.join(outputRoot, "coverage.md"), renderMarkdown(coverage)),
    Bun.write(path.join(outputRoot, "coverage.json"), JSON.stringify(coverageJSON(coverage), null, 2) + "\n"),
  ])
}

function renderMarkdown(coverage: Coverage) {
  return (
    [
      "# Coverage",
      "",
      coverageTotals(coverage),
      "",
      "## Operations",
      "",
      "| covered | scenarios | operation |",
      "| --- | --- | --- |",
      ...coverage.operations.map((item) =>
        row(mark(item), item.scenarios.join(", "), `\`${item.method} ${item.path}\` (${item.id})`),
      ),
      "",
      "## Event types",
      "",
      "| covered | scenarios | event |",
      "| --- | --- | --- |",
      ...coverage.events.map((item) => row(mark(item), item.scenarios.join(", "), `\`${item.id}\``)),
      "",
      "## WebSocket routes",
      "",
      "| covered | scenarios | websocket |",
      "| --- | --- | --- |",
      ...coverage.websockets.map((item) =>
        row(mark(item), item.scenarios.join(", "), `\`${item.method} ${item.path}\` (${item.id})`),
      ),
      "",
      "## Unmatched requests",
      "",
      ...(coverage.unmatched.length === 0 ? ["none"] : coverage.unmatched),
      "",
    ].join("\n") + "\n"
  )
}

function row(...cells: string[]) {
  return `| ${cells.join(" | ")} |`
}

function mark(item: CoverageItem) {
  if (!item.compared) return "not compared"
  return item.covered ? "yes" : "no"
}

function coverageJSON(coverage: Coverage) {
  return {
    operations: coverage.operations.map(operationJSON),
    events: coverage.events.map((item) => ({
      id: item.id,
      covered: item.covered,
      scenarios: item.scenarios,
      compared: item.compared,
    })),
    websockets: coverage.websockets.map(operationJSON),
    unmatched: coverage.unmatched,
  }
}

function operationJSON(item: CoverageItem) {
  return {
    id: item.id,
    method: item.method,
    path: item.path,
    covered: item.covered,
    scenarios: item.scenarios,
    compared: item.compared,
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}
