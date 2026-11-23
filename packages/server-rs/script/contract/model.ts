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

export async function contractRoutes(): Promise<Array<Route>> {
  const routes: Array<Route> = []
  const operationIds = new Set<string>()
  HttpApi.reflect(api, {
    onGroup() {},
    onEndpoint({ endpoint, group, middleware, successes, errors }) {
      const operationId = Context.getOrElse(endpoint.annotations, OpenApi.Identifier, () =>
        group.topLevel ? endpoint.identifier : `${group.identifier}.${endpoint.identifier}`,
      )
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
