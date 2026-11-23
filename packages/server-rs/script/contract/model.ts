import { Context, Schema, SchemaAST, SchemaRepresentation } from "effect"
import { HttpApi, HttpApiMiddleware, HttpApiSchema, OpenApi } from "effect/http-api"
import { upstreamTree } from "../upstream"

const tree = await upstreamTree()

const protocol = await import(`${tree}/packages/protocol/src/api.ts`)

class LocationMiddleware extends HttpApiMiddleware.Service<LocationMiddleware>()("@opencode/server-rs/Location", {}) {}
class FormMiddleware extends HttpApiMiddleware.Service<FormMiddleware>()("@opencode/server-rs/Form", {}) {}
class SessionMiddleware extends HttpApiMiddleware.Service<SessionMiddleware>()("@opencode/server-rs/Session", {}) {}

const api = protocol.makeDefaultApi({
  locationMiddleware: LocationMiddleware,
  formLocationMiddleware: FormMiddleware,
  sessionLocationMiddleware: SessionMiddleware,
})

export type Route = {
  operationId: string
  identifier: string
  group: string
  tag: string
  method: string
  path: string
  scope: "Global" | "Location" | "Session" | "Form"
  stability: "Stable" | "Experimental"
  body: "None" | "Json" | "Binary"
  success: "Json" | "Data" | "Empty" | "Binary" | "Sse" | "WebSocket"
  successStatus: number
  wildcard: boolean
  errors: Array<{ tag: string; status: number }>
}

const scopeKeys = {
  Session: "@opencode/server-rs/Session",
  Form: "@opencode/server-rs/Form",
  Location: "@opencode/server-rs/Location",
} as const

export type ContractError = { tag: string; status: number; schema: Schema.Top }

export type EventRoot = { type: string; identifier: string; schema: Schema.Top }

export type DurableRoot = EventRoot & { key: string; version: number; aggregate: string }

export type IdRoot = {
  identifier: string
  schema: Schema.Top
  wire: Schema.Top
  transform: "trim" | undefined
}

export type ResponseRoot = { operationId: string; success: Route["success"]; schema: Schema.Top }

type DurableDefinition = Schema.Top & {
  readonly type: string
  readonly durable: { readonly version: number; readonly aggregate: string }
}

const brandedId = /ID$/

export async function contractEvents(): Promise<{
  publicUnion: Schema.Top
  sessionLog: Schema.Top
  public: Array<EventRoot>
  durable: Array<DurableRoot>
  sessionLogTypes: Array<string>
  ids: Array<IdRoot>
}> {
  const protocolEvents = await import(`${tree}/packages/protocol/src/groups/event.ts`)
  const eventManifest = await import(`${tree}/packages/schema/src/event-manifest.ts`)
  const sessionEvents = await import(`${tree}/packages/schema/src/session-event.ts`)
  const eventLog = await import(`${tree}/packages/schema/src/event-log.ts`)

  const members: ReadonlyArray<SchemaAST.AST> = SchemaAST.isUnion(protocolEvents.OpenCodeEvent.ast)
    ? protocolEvents.OpenCodeEvent.ast.types
    : []
  const publicEvents: Array<EventRoot> = members.map((member) => {
    const type = eventType(member)
    if (type === undefined) throw new Error("contract: public event member has no type tag")
    return { type, identifier: SchemaAST.resolveIdentifier(member) ?? type, schema: Schema.make(member) }
  })

  const durable = [...(eventManifest.Durable as ReadonlyMap<string, DurableDefinition>).entries()].map(([key, value]) => ({
    key,
    type: value.type,
    identifier: SchemaAST.resolveIdentifier(value.ast) ?? value.type,
    version: value.durable.version,
    aggregate: value.durable.aggregate,
    schema: value,
  }))

  const synced = eventType(eventLog.Synced.ast)
  if (synced === undefined) throw new Error("contract: log.synced has no type tag")
  const sessionLogTypes = [
    ...(sessionEvents.DurableDefinitions as ReadonlyArray<DurableDefinition>).map((definition) => definition.type),
    synced,
  ]

  return {
    publicUnion: protocolEvents.OpenCodeEvent,
    public: publicEvents,
    durable,
    sessionLog: sessionLogItem(),
    sessionLogTypes,
    ids: await contractIds([...publicEvents.map((event) => event.schema), ...durable.map((event) => event.schema)]),
  }
}

function sessionLogItem(): Schema.Top {
  let data: SchemaAST.AST | undefined
  reflect({
    onEndpoint({ endpoint }) {
      if (endpoint.path !== "/api/experimental/session/:sessionID/log") return
      const success = [...endpoint.success][0]
      if (success === undefined) throw new Error("contract: session.log has no success schema")
      data = sseData(success)
    },
  })
  if (data === undefined) throw new Error("contract: session.log is missing from the API")
  return Schema.make<Schema.Top>(data)
}

async function contractIds(events: ReadonlyArray<Schema.Top>): Promise<Array<IdRoot>> {
  const asts = [...contractApiAsts(), ...events.map((schema) => schema.ast)]
  const found = new Map<string, SchemaAST.AST>()
  for (const ast of asts) visit(ast, found)
  return [...found.entries()]
    .toSorted(([left], [right]) => (left < right ? -1 : 1))
    .map(([identifier, ast]) => branded(identifier, ast))
}

function branded(identifier: string, ast: SchemaAST.AST): IdRoot {
  const schema = Schema.make<Schema.Top>(ast)
  const encoded = Schema.toEncoded(schema).ast
  const wire = Schema.make<Schema.Top>(encoded)
  return {
    identifier,
    schema,
    wire: SchemaAST.resolveIdentifier(encoded) === undefined ? wire.annotate({ identifier }) : wire,
    transform: idTransform(identifier, ast),
  }
}

function idTransform(identifier: string, ast: SchemaAST.AST): "trim" | undefined {
  if (ast._tag !== "String" || ast.encoding === undefined) return undefined
  const filters = (checks: ReadonlyArray<SchemaAST.Check<unknown>>): Array<SchemaAST.Filter<unknown>> =>
    checks.flatMap((check) => (check._tag === "FilterGroup" ? filters(check.checks) : [check]))
  const checks = filters(ast.checks ?? [])
  const trimmed = checks.some((filter) => filter.annotations?.representation?.id === "effect/schema/isTrimmed")
  const nonEmpty = checks.some((filter) => {
    if (filter.annotations?.representation?.id !== "effect/schema/isMinLength") return false
    const payload = filter.annotations.representation.payload as { readonly minLength?: unknown } | null | undefined
    return payload?.minLength === 1
  })
  const link = ast.encoding.length === 1 ? ast.encoding[0] : undefined
  if (trimmed && nonEmpty && link?.transformation._tag === "Transformation") {
    if (link.transformation.decode._tag === "Transform" && link.transformation.encode._tag === "Passthrough") {
      return "trim"
    }
  }
  throw new Error(`contract: ${identifier} has a transformation the generator does not model`)
}

function contractApiAsts(): Array<SchemaAST.AST> {
  const asts: Array<SchemaAST.AST> = []
  reflect({
    onEndpoint({ endpoint, successes, errors }) {
      if (endpoint.params !== undefined) asts.push(endpoint.params.ast)
      for (const item of endpoint.payload.values()) for (const schema of item.schemas) asts.push(schema.ast)
      for (const schema of endpoint.success) asts.push(schema.ast)
      for (const schema of endpoint.error) asts.push(schema.ast)
      for (const group of successes.values()) for (const schema of group) asts.push(schema.ast)
      for (const group of errors.values()) for (const schema of group) asts.push(schema.ast)
    },
  })
  return asts
}

function visit(ast: SchemaAST.AST, found: Map<string, SchemaAST.AST>, seen = new Set<SchemaAST.AST>()) {
  if (seen.has(ast)) return
  seen.add(ast)
  const identifier = SchemaAST.resolveIdentifier(ast)
  if (identifier !== undefined && brandedId.test(identifier) && !found.has(identifier)) found.set(identifier, ast)
  if (!("recur" in ast) || typeof ast.recur !== "function") return
  ast.recur((child: SchemaAST.AST) => {
    visit(child, found, seen)
    return child
  })
}

function eventType(ast: SchemaAST.AST) {
  if (!SchemaAST.isObjects(ast)) return undefined
  const property = ast.propertySignatures.find((field) => String(field.name) === "type")
  if (property === undefined) return undefined
  const type = property.type
  if (type._tag === "Literal" && typeof type.literal === "string") return type.literal
  if (type._tag === "TemplateLiteral" && type.parts.length === 2) {
    const [head, tail] = type.parts
    if (head?._tag === "Literal" && typeof head.literal === "string" && tail?._tag === "String") {
      return `${head.literal}\${string}`
    }
  }
  return undefined
}

type ReflectOptions = Parameters<typeof HttpApi.reflect>[1]
type EndpointReflection = Parameters<NonNullable<ReflectOptions["onEndpoint"]>>[0]

function reflect(options: Pick<ReflectOptions, "onEndpoint">) {
  HttpApi.reflect(api, { onGroup() {}, ...options })
}

function operationIdOf({ endpoint, group }: EndpointReflection) {
  return Context.getOrElse(endpoint.annotations, OpenApi.Identifier, () =>
    group.topLevel ? endpoint.identifier : `${group.identifier}.${endpoint.identifier}`,
  )
}

export async function contractErrors(): Promise<Array<ContractError>> {
  const found = new Map<string, ContractError>()
  reflect({
    onEndpoint({ errors }) {
      for (const [status, schemas] of errors) {
        for (const schema of schemas) {
          const tag = SchemaAST.resolveIdentifier(schema.ast)
          if (tag === undefined) throw new Error(`contract: error ${status} has no schema identifier`)
          const existing = found.get(tag)
          if (existing !== undefined) {
            if (existing.status !== status)
              throw new Error(`contract: ${tag} is declared with status ${existing.status} and ${status}`)
            continue
          }
          found.set(tag, { tag, status, schema })
        }
      }
    },
  })
  return [...found.values()].toSorted((a, b) => (a.tag < b.tag ? -1 : 1))
}

export async function contractRoutes(): Promise<Array<Route>> {
  const routes: Array<Route> = []
  const operationIds = new Set<string>()
  reflect({
    onEndpoint(reflection) {
      const { endpoint, group, middleware, successes, errors } = reflection
      const operationId = operationIdOf(reflection)
      if (operationIds.has(operationId)) throw new Error(`contract: duplicate operation id ${operationId}`)
      operationIds.add(operationId)
      const title = Context.getOrElse(group.annotations, OpenApi.Title, () => group.identifier)
      const transform = Context.getOrElse(
        endpoint.annotations,
        OpenApi.Transform,
        () => (operation: Record<string, unknown>) => operation,
      )
      const operation = transform({ tags: [title] })
      const tag = (operation.tags as ReadonlyArray<string> | undefined)?.[0] ?? title
      const keys = [...middleware].map((item) => item.key)
      routes.push({
        operationId,
        identifier: endpoint.identifier,
        group: group.identifier,
        tag,
        method: endpoint.method,
        path: openApiPath(endpoint.path),
        scope: scopeOf(keys),
        stability: isExperimental(endpoint.path) ? "Experimental" : "Stable",
        body: bodyKind(endpoint.payload),
        ...successKind(endpoint, successes, Object.hasOwn(operation, "x-websocket")),
        wildcard: endpoint.path.endsWith("*"),
        errors: errorTags(operationId, errors),
      })
    },
  })
  return routes
}

export async function contractResponses(events: Awaited<ReturnType<typeof contractEvents>>): Promise<Array<ResponseRoot>> {
  const routes = await contractRoutes()
  const kinds = new Map(routes.map((route) => [route.operationId, route.success]))
  const responses = new Map<string, ResponseRoot>()
  reflect({
    onEndpoint(reflection) {
      const { endpoint } = reflection
      const operationId = operationIdOf(reflection)
      const success = kinds.get(operationId)
      if (success === undefined) throw new Error(`contract: ${operationId} is missing a route`)
      const first = [...endpoint.success][0]
      if (first === undefined) throw new Error(`contract: ${operationId} has no success schema`)
      if (success === "Json" || success === "Data") responses.set(operationId, { operationId, success, schema: first })
      if (success === "Sse")
        responses.set(operationId, { operationId, success, schema: sseResponse(first, events) })
    },
  })
  return routes.flatMap((route) => {
    const response = responses.get(route.operationId)
    return response === undefined ? [] : [response]
  })
}

function sseResponse(schema: Schema.Top, events: Awaited<ReturnType<typeof contractEvents>>) {
  const data = Schema.make<Schema.Top>(sseData(schema))
  const identifier = SchemaAST.resolveIdentifier(data.ast)
  if (identifier === "V2Event") return events.publicUnion
  if (identifier === "SessionLogItem") return events.sessionLog
  throw new Error(`contract: SSE payload ${identifier ?? "without an identifier"} has no canonical schema`)
}

function openApiPath(path: string) {
  return path.replace(/\/:([A-Za-z0-9_]+)/g, "/{$1}")
}

function isExperimental(path: string) {
  return path === "/api/experimental" || path.startsWith("/api/experimental/")
}

function scopeOf(keys: ReadonlyArray<string>): Route["scope"] {
  if (keys.includes(scopeKeys.Session)) return "Session"
  if (keys.includes(scopeKeys.Form)) return "Form"
  if (keys.includes(scopeKeys.Location)) return "Location"
  return "Global"
}

function bodyKind(payload: {
  readonly size: number
  readonly values: () => Iterable<{ readonly schemas: ReadonlyArray<Schema.Top> }>
}): Route["body"] {
  if (payload.size === 0) return "None"
  const schema = [...payload.values()][0]?.schemas[0]
  if (schema === undefined) return "None"
  return encodingTag(schema) === "Uint8Array" ? "Binary" : "Json"
}

function successKind(
  endpoint: { readonly success: ReadonlySet<Schema.Top> },
  successes: ReadonlyMap<number, readonly [Schema.Top, ...Array<Schema.Top>]>,
  websocket: boolean,
): Pick<Route, "success" | "successStatus"> {
  const first = [...endpoint.success][0]
  if (first === undefined) throw new Error("contract: endpoint has no success schema")
  const stream = (first as { readonly _tag?: string })._tag === "StreamSse"
  const [status, schemas] = [...successes][0] ?? [200, [first] as const]
  const schema = stream ? first : (schemas[0] ?? first)
  const success = websocket
    ? "WebSocket"
    : stream
      ? "Sse"
      : HttpApiSchema.isNoContent(schema.ast)
        ? "Empty"
        : encodingTag(schema) === "Uint8Array"
          ? "Binary"
          : isDataEnvelope(schema)
            ? "Data"
            : "Json"
  return { success, successStatus: stream ? 200 : status }
}

function errorTags(operationId: string, errors: ReadonlyMap<number, readonly [Schema.Top, ...Array<Schema.Top>]>) {
  return [...errors].flatMap(([status, schemas]) =>
    schemas.map((schema) => {
      const tag = SchemaAST.resolveIdentifier(schema.ast)
      if (tag === undefined) throw new Error(`contract: ${operationId} error ${status} has no schema identifier`)
      return { tag, status }
    }),
  )
}

function encodingTag(schema: Schema.Top) {
  return SchemaAST.resolveAt<{ readonly _tag: string }>("~httpApiEncoding")(schema.ast)?._tag
}

function isDataEnvelope(schema: Schema.Top) {
  if (isStreamSchema(schema) || HttpApiSchema.isNoContent(schema.ast)) return false
  const ast = Schema.toType(schema).ast
  return (
    SchemaAST.isObjects(ast) &&
    ast.indexSignatures.length === 0 &&
    ast.propertySignatures.length === 1 &&
    ast.propertySignatures[0]?.name === "data"
  )
}

function isStreamSchema(schema: Schema.Top) {
  return "_tag" in schema && (schema._tag === "StreamSse" || schema._tag === "StreamUint8Array")
}

export function sseData(schema: Schema.Top) {
  const events = (schema as unknown as { readonly events: Schema.Top }).events.ast
  if (!SchemaAST.isObjects(events)) throw new Error("Invalid SSE data schema")
  const data = events.propertySignatures.find((field) => field.name === "data")!.type
  const replaceEncoding = Reflect.get(SchemaAST, "replaceEncoding") as (
    ast: SchemaAST.AST,
    encoding: undefined,
  ) => SchemaAST.AST
  return Schema.toEncoded(Schema.make<Schema.Top>(replaceEncoding(data, undefined))).ast
}

export function normalize(input: ReadonlyArray<SchemaAST.AST>) {
  const cache = new WeakMap<SchemaAST.AST, SchemaAST.AST>()
  const references = new Map<string, SchemaAST.AST>()
  const recur = (ast: SchemaAST.AST): SchemaAST.AST => {
    const cached = cache.get(ast)
    if (cached !== undefined) return cached
    const identifier = SchemaAST.resolveIdentifier(ast)
    const output = step(ast)
    const key =
      identifier === undefined
        ? undefined
        : `${identifier}\0${ast.context?.isOptional === true}\0${JSON.stringify(SchemaRepresentation.toJson(SchemaRepresentation.toRepresentation(output)))}`
    const reference = key === undefined ? undefined : references.get(key)
    if (reference !== undefined) {
      cache.set(ast, reference)
      return reference
    }
    cache.set(ast, output)
    if (key !== undefined) references.set(key, output)
    return output
  }
  const step = (ast: SchemaAST.AST): SchemaAST.AST => {
    if (SchemaAST.isDeclaration(ast) && ast.annotations?.toCode === undefined) {
      const representation = ast.annotations?.representation
      if (
        typeof representation === "object" &&
        representation !== null &&
        "id" in representation &&
        representation.id === "effect/schema/Json"
      ) {
        return Schema.Json.ast
      }
      if (ast.annotations?.["~constructor"] !== undefined && ast.typeParameters[0] !== undefined) {
        const identifier = SchemaAST.resolveIdentifier(ast)
        const fields = recur(ast.typeParameters[0])
        if (identifier === undefined) return fields
        return Schema.make<Schema.Top>(fields).annotate({ identifier }).ast
      }
    }
    if (!("recur" in ast) || typeof ast.recur !== "function") return ast
    return ast.recur(recur)
  }
  return input.map(recur)
}
