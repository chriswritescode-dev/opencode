import { Cause, Effect, Exit, Layer, Option, Schema, Stream } from "effect"
import { HttpClientRequest, HttpClientResponse } from "effect/http"
import { Socket } from "effect/socket"

export const FIXED_EPOCH_MS: number = Date.UTC(2026, 0, 2, 3, 4, 5)

class FixedDate extends Date {
  constructor(...args: Array<any>) {
    if (args.length === 0) {
      super(FIXED_EPOCH_MS)
      return
    }
    super(...(args as [number]))
  }
  static override now(): number {
    return FIXED_EPOCH_MS
  }
}

let uuidSequence = 1
const nextUUID = () => `00000000-0000-4000-8000-${String(uuidSequence++).padStart(12, "0")}`

const resetUUID = () => {
  uuidSequence = 1
}

globalThis.Date = FixedDate as unknown as DateConstructor
Object.defineProperty(globalThis.crypto, "randomUUID", {
  configurable: true,
  writable: true,
  value: nextUUID,
})

const FORBIDDEN_ENV_PREFIXES = [
  "AWS_",
  "ANTHROPIC_",
  "OPENAI_",
  "GOOGLE_",
  "GEMINI_",
  "CEREBRAS_",
  "DEEPINFRA_",
  "TOGETHER",
  "VENICE_",
  "AI_GATEWAY_",
  "BEDROCK_",
]
for (const name of Object.keys(process.env)) {
  if (FORBIDDEN_ENV_PREFIXES.some((prefix) => name.startsWith(prefix))) delete process.env[name]
}

export type Loader = <T = any>(spec: string) => Promise<T>

export function createLoader(tree: string): Loader {
  const cache = new Map<string, Promise<unknown>>()
  return <T = any>(spec: string): Promise<T> => {
    const cached = cache.get(spec)
    if (cached !== undefined) return cached as Promise<T>
    const path =
      spec.startsWith(".") || spec.startsWith("/") || spec.startsWith("packages/")
        ? `${tree}/${spec}`
        : Bun.resolveSync(spec, `${tree}/packages/ai`)
    const imported = import(path)
    cache.set(spec, imported)
    return imported as Promise<T>
  }
}

export type AuthDescriptor =
  | { readonly type: "bearer"; readonly value: string }
  | { readonly type: "header"; readonly name: string; readonly value: string }
  | {
      readonly type: "sigv4"
      readonly region: string
      readonly accessKeyId: string
      readonly secretAccessKey: string
      readonly sessionToken?: string
    }
  | { readonly type: "none" }

export interface ProviderStep {
  readonly package: string
  readonly modelID: string
  readonly settings: Record<string, unknown>
}

export interface RedactOptions {
  readonly headers?: ReadonlyArray<string>
  readonly allowRequestHeaders?: ReadonlyArray<string>
  readonly allowResponseHeaders?: ReadonlyArray<string>
  readonly queryParameters?: ReadonlyArray<string>
  readonly jsonFields?: ReadonlyArray<string>
  readonly url?: (url: string) => string
  readonly body?: (body: string) => string
}

export interface CapturedRequest {
  readonly method: string
  readonly url: string
  readonly headers: Record<string, string>
  readonly body: string
  readonly bodyEncoding?: "base64"
}

export interface CapturedResponse {
  readonly status: number
  readonly headers: Record<string, string>
  readonly body: string
  readonly bodyEncoding?: "base64"
}

export interface CapturedExchange {
  readonly prepared: CapturedRequest
  readonly response: CapturedResponse
}

export interface CapturedStep {
  readonly model: Record<string, unknown>
  readonly request: Record<string, unknown>
  readonly provider?: ProviderStep
  readonly transport: "http" | "openai-responses-channel"
  readonly exchanges?: ReadonlyArray<CapturedExchange>
  readonly websocket?: {
    readonly connect?: { readonly url: string; readonly headers: Record<string, string> }
    readonly ops: ReadonlyArray<unknown>
    readonly sent: ReadonlyArray<string>
    readonly received: ReadonlyArray<unknown>
  }
  readonly events?: ReadonlyArray<unknown>
  readonly error?: { readonly reason: unknown; readonly retryable: boolean }
}

export interface CapturedCassette {
  readonly cassette: string
  readonly protocol: string
  readonly routeId: string
  readonly clockEpochMs: number
  readonly steps: ReadonlyArray<CapturedStep>
}

export interface ChannelController {
  readonly executor: any
  readonly sent: ReadonlyArray<string>
  readonly opens: () => number
  readonly reconnect: (preserveCheckpoint?: boolean) => any
}

export interface ScenarioContext {
  readonly load: Loader
  readonly channel: ChannelController
}

export interface Scenario {
  readonly cassette: string
  readonly protocol: string
  readonly auth?: AuthDescriptor
  readonly provider?: ProviderStep
  readonly descriptorOnly?: string
  readonly transport?: "http" | "websocket"
  readonly directory?: string
  readonly redact?: RedactOptions
  readonly run: (context: ScenarioContext) => any
}

interface StepRecord {
  model: Record<string, unknown>
  request: Record<string, unknown>
  provider?: ProviderStep
  transport: "http" | "openai-responses-channel"
  exchanges?: Array<CapturedExchange>
  websocket?: {
    connect?: { url: string; headers: Record<string, string> }
    ops: Array<unknown>
    sent: Array<string>
    received: Array<unknown>
  }
  events?: Array<unknown>
  error?: { reason: unknown; retryable: boolean }
  raw: Array<unknown>
}

const PROTOCOLS: Record<string, { readonly path: string; readonly transport: string }> = {
  "anthropic-messages": { path: "packages/ai/src/protocols/anthropic-messages.ts", transport: "http-json" },
  "openai-chat": { path: "packages/ai/src/protocols/openai-chat.ts", transport: "http-json/sse" },
  "open-responses": { path: "packages/ai/src/protocols/open-responses.ts", transport: "http-json/sse" },
  "openai-responses": { path: "packages/ai/src/protocols/openai-responses.ts", transport: "http-json/sse" },
  gemini: { path: "packages/ai/src/protocols/gemini.ts", transport: "http-json/sse" },
  "bedrock-converse": { path: "packages/ai/src/protocols/bedrock-converse.ts", transport: "http-json" },
}

const TEXT_CONTENT_TYPES = new Set([
  "application/graphql",
  "application/javascript",
  "application/json",
  "application/sql",
  "application/x-www-form-urlencoded",
  "application/xml",
  "application/yaml",
  "image/svg+xml",
])

const isTextContentType = (contentType: string | undefined) => {
  const mediaType = contentType?.split(";", 1)[0]?.trim().toLowerCase()
  if (!mediaType) return false
  return (
    mediaType.startsWith("text/") ||
    mediaType.endsWith("+json") ||
    mediaType.endsWith("+xml") ||
    TEXT_CONTENT_TYPES.has(mediaType)
  )
}

const roundTrip = (value: unknown): any =>
  JSON.parse(
    JSON.stringify(value, (_key, item) => (item instanceof Uint8Array ? Buffer.from(item).toString("base64") : item)),
  )

const normalizeModel = (value: unknown) => {
  const model = roundTrip(value)
  for (const key of ["options", "modelOptions"]) {
    const options = model[key]
    if (options === undefined) continue
    if (options.providerOptions !== undefined && Object.keys(options.providerOptions).length === 0)
      delete options.providerOptions
    if (Object.keys(options).length === 0) delete model[key]
  }
  return model
}

const providerAuthHeader = new Map([
  ["@opencode/ai/providers/anthropic", "x-api-key"],
  ["@opencode/ai/providers/anthropic-compatible", "x-api-key"],
  ["@opencode/ai/providers/google", "x-goog-api-key"],
])

const describeAuth = (provider: ProviderStep): AuthDescriptor => {
  const settings = provider.settings as Record<string, any>
  if (provider.package === "@opencode/ai/providers/amazon-bedrock") {
    const credentials = settings.credentials
    if (credentials !== null && typeof credentials === "object") {
      return {
        type: "sigv4",
        region: settings.region ?? credentials.region ?? "us-east-1",
        accessKeyId: credentials.accessKeyId,
        secretAccessKey: credentials.secretAccessKey,
        ...(credentials.sessionToken === undefined ? {} : { sessionToken: credentials.sessionToken }),
      }
    }
  }
  if (settings.apiKey === false) return { type: "none" }
  if (typeof settings.apiKey === "string") {
    const header = providerAuthHeader.get(provider.package)
    if (header !== undefined) return { type: "header", name: header, value: settings.apiKey }
    return { type: "bearer", value: settings.apiKey }
  }
  if (typeof settings.authToken === "string") return { type: "bearer", value: settings.authToken }
  return { type: "none" }
}

const providerModel = async (load: Loader, tree: string, provider: ProviderStep) => {
  const settings = JSON.parse(JSON.stringify(provider.settings)) as Record<string, unknown>
  if (JSON.stringify(settings) !== JSON.stringify(provider.settings))
    throw new Error(`capture: provider settings for ${provider.package} do not survive JSON`)
  const manifest = await Bun.file(`${tree}/packages/ai/package.json`).json()
  const key = provider.package.replace(/^@opencode\/ai\//, "./")
  const target = manifest.exports?.[key]
  if (typeof target !== "string") throw new Error(`capture: ${provider.package} is not exported by the pin`)
  const module = await load(`packages/ai/${target.replace(/^\.\//, "")}`)
  return module.model(provider.modelID, settings)
}

let active: StepRecord | undefined
let steps: Array<StepRecord> = []
const pendingChannelOps: Array<unknown> = []

const captureHttp = (request: any, next: any) =>
  Effect.gen(function* () {
    const step = active
    if (step === undefined) throw new Error("capture: HTTP request outside a captured step")
    const web = yield* HttpClientRequest.toWeb(request)
    const body = yield* Effect.promise(() => web.text())
    const prepared: CapturedRequest = {
      method: web.method,
      url: web.url,
      headers: Object.fromEntries(web.headers.entries()),
      body,
    }
    const response = yield* next(request)
    const bytes = yield* response.arrayBuffer
    const text = isTextContentType(response.headers["content-type"])
    const captured: CapturedResponse = {
      status: response.status,
      headers: Object.fromEntries(Object.entries(response.headers)),
      body: text ? new TextDecoder().decode(bytes) : Buffer.from(bytes).toString("base64"),
      ...(text ? {} : { bodyEncoding: "base64" as const }),
    }
    step.exchanges ??= []
    step.exchanges.push({ prepared, response: captured })
    return HttpClientResponse.fromWeb(
      request,
      new Response(bytes, { status: response.status, headers: response.headers }),
    )
  })

export async function capture(tree: string, scenario: Scenario): Promise<CapturedCassette> {
  const registered = PROTOCOLS[scenario.protocol]
  if (registered === undefined) throw new Error(`capture: no protocol module is registered for "${scenario.protocol}"`)

  const load = createLoader(tree)
  const built = scenario.provider === undefined ? undefined : await providerModel(load, tree, scenario.provider)
  const auth: AuthDescriptor =
    scenario.auth ?? (scenario.provider === undefined ? { type: "none" } : describeAuth(scenario.provider))
  process.env.CI = "true"

  const [httpRecorder, platformNode, route, schema] = await Promise.all([
    load("@opencode/http-recorder"),
    load("@effect/platform-node"),
    load("packages/ai/src/route.ts"),
    load("packages/ai/src/schema/index.ts"),
  ])
  const HttpRecorder = httpRecorder.HttpRecorder
  const { LLMClient, RequestExecutor, WebSocketTransport } = route
  const directory = scenario.directory ?? `${tree}/packages/ai/test/fixtures/recordings`

  steps = []
  active = undefined
  pendingChannelOps.length = 0

  const encode = (codec: any, value: unknown) => roundTrip(Schema.encodeSync(codec)(value))

  const encodeError = (error: any) => {
    const reason = encode(schema.AIErrorReason, error.reason)
    delete reason.cause
    return { reason, retryable: error.isRetryable }
  }

  const encodeRequest = (request: any) => {
    if (request.fetch !== undefined) throw new Error("capture: request.fetch is not captured")
    const { system, messages, tools, toolChoice } = request
    const rest: Record<string, unknown> = { ...request }
    delete rest.model
    delete rest.fetch
    delete rest.system
    delete rest.messages
    delete rest.tools
    delete rest.toolChoice
    return roundTrip({
      ...rest,
      system: system.map((part: unknown) => encode(schema.SystemPart, part)),
      messages: messages.map((message: unknown) => encode(schema.Message, message)),
      tools: tools.map((tool: unknown) => encode(schema.ToolEntry, tool)),
      ...(toolChoice === undefined ? {} : { toolChoice: encode(schema.ToolChoice, toolChoice) }),
    })
  }

  const encodeModel = (model: any, transport: StepRecord["transport"], auth: AuthDescriptor) =>
    normalizeModel({
      id: model.id,
      provider: model.provider,
      route: {
        id: model.route.id,
        protocol: model.route.protocol,
        providerMetadataKey: model.route.providerMetadataKey,
      },
      endpoint: {
        baseURL: model.route.endpoint.baseURL,
        path: typeof model.route.endpoint.path === "string" ? model.route.endpoint.path : null,
        query: model.route.endpoint.query,
      },
      options: model.route.options,
      modelOptions: model.options,
      compatibility: model.compatibility,
      capabilities: model.capabilities,
      transport,
      auth,
    })

  const begin = (request: any, options: any): StepRecord => {
    resetUUID()
    const transport = options?.webSocket === undefined ? "http" : "openai-responses-channel"
    const step: StepRecord = {
      model: encodeModel(request.model, transport, auth),
      request: encodeRequest(request),
      provider: scenario.provider,
      transport,
      raw: [],
    }
    if (transport === "http") step.exchanges = []
    else step.websocket = { ops: [...pendingChannelOps.splice(0)], sent: [], received: [] }
    steps.push(step)
    active = step
    return step
  }

  const finish = (step: StepRecord, exit: Exit.Exit<unknown, unknown>) => {
    active = undefined
    if (Exit.isSuccess(exit)) {
      step.events = step.raw.map((event) => encode(schema.LLMEvent, event))
      return
    }
    delete step.events
    const error = Option.getOrUndefined(Cause.findErrorOption(exit.cause))
    if (!(error instanceof schema.AIError)) throw new Error(Cause.pretty(exit.cause))
    step.error = encodeError(error)
  }

  const wrappedInterface = (inner: any) => ({
    stream: (request: any, options: any) =>
      Stream.suspend(() => {
        const step = begin(request, options)
        return inner.stream(request, options).pipe(
          Stream.tap((event: unknown) => Effect.sync(() => step.raw.push(event))),
          Stream.onExit((exit) => Effect.sync(() => finish(step, exit))),
        )
      }),
    generate: (request: any, options: any) =>
      Effect.suspend(() => {
        const step = begin(request, options)
        return inner.generate(request, options).pipe(
          Effect.tap((response: any) => Effect.sync(() => step.raw.push(...response.events))),
          Effect.onExit((exit) => Effect.sync(() => finish(step, exit))),
        )
      }),
    compact: (request: any, options: any) => inner.compact(request, options),
  })

  const terminal = (observation: any) => observation.type !== "frame"

  const observationFrame = (observation: any) => {
    if (observation.type === "frame" || observation.type === "completed" || observation.type === "incomplete")
      return Effect.succeed(observation.frame)
    return Effect.fail(observation.error)
  }

  const recordObservation = (step: StepRecord | undefined, observation: any) => {
    if (step?.websocket === undefined) return
    if (observation.type === "frame" || observation.type === "completed" || observation.type === "incomplete")
      return void step.websocket.received.push({ type: observation.type, frame: observation.frame })
    if (observation.type === "rejected")
      return void step.websocket.received.push({
        type: observation.type,
        recovery: observation.recovery,
        ...encodeError(observation.error),
      })
    step.websocket.received.push({ type: observation.type, ...encodeError(observation.error) })
  }

  const makeChannel = Effect.gen(function* () {
    const constructor = yield* Socket.WebSocketConstructor
    let connection: any
    let checkpoint: any
    let pending: any
    let opens = 0
    const sent: Array<string> = []

    const close: Effect.Effect<void> = Effect.suspend(() => {
      const current = connection
      connection = undefined
      return current ? current.close : Effect.void
    })
    yield* Effect.addFinalizer(() => close)

    const executor = {
      execute: (exchange: any) =>
        Effect.gen(function* () {
          if (!connection) {
            connection = yield* WebSocketTransport.open(exchange.connect).pipe(
              Effect.provideService(Socket.WebSocketConstructor, constructor),
            )
            opens += 1
            const connecting = active
            if (connecting?.websocket !== undefined)
              connecting.websocket.connect = {
                url: exchange.connect.url,
                headers: Object.fromEntries(Object.entries(exchange.connect.headers)),
              }
          }
          const current = connection
          const create = yield* exchange.driver.create(checkpoint)
          if (create.mode === "full") checkpoint = undefined
          pending = undefined
          const step = active
          if (step?.websocket !== undefined) step.websocket.sent.push(create.message)
          sent.push(create.message)
          yield* current.sendText(create.message)
          const decoder = new TextDecoder()
          return {
            frames: current.messages.pipe(
              Stream.map((message: unknown) => WebSocketTransport.messageText(message, decoder)),
              Stream.mapEffect((frame: string) => exchange.driver.observe(create, frame)),
              Stream.tap((observation: unknown) => Effect.sync(() => recordObservation(active, observation))),
              Stream.tap((observation: any) =>
                Effect.sync(() => {
                  if (!terminal(observation)) return
                  pending = observation.type === "completed" ? observation.checkpoint : undefined
                  if (observation.type !== "completed") checkpoint = undefined
                }),
              ),
              Stream.takeUntil(terminal),
              Stream.mapEffect(observationFrame),
            ),
            complete: Effect.sync(() => {
              checkpoint = pending
              pending = undefined
            }),
          }
        }),
    }

    const controller: ChannelController = {
      executor,
      sent,
      opens: () => opens,
      reconnect: (preserveCheckpoint = false) =>
        close.pipe(
          Effect.andThen(
            Effect.sync(() => {
              pending = undefined
              if (!preserveCheckpoint) checkpoint = undefined
              pendingChannelOps.push({ type: "reconnect", preserveCheckpoint })
            }),
          ),
        ),
    }

    return controller
  })

  const executor = RequestExecutor.layer.pipe(
    Layer.provide(HttpRecorder.layerFetch(scenario.cassette, { directory, redact: scenario.redact })),
  )
  const capturing = RequestExecutor.middleware(captureHttp, executor)
  const base = LLMClient.layer.pipe(Layer.provide(capturing))
  const wrapped = Layer.effect(
    LLMClient.Service,
    Effect.gen(function* () {
      const inner = yield* LLMClient.Service
      return LLMClient.Service.of(wrappedInterface(inner))
    }),
  ).pipe(Layer.provide(base))
  const webSocket = HttpRecorder.layerWebSocketConstructor(scenario.cassette, { directory }).pipe(
    Layer.provide(platformNode.NodeWS.layerWebSocketConstructor),
  )
  const layer = Layer.mergeAll(wrapped, webSocket)

  const program = Effect.gen(function* () {
    const channel = yield* makeChannel
    yield* scenario.run({ load, channel }) as Effect.Effect<void, unknown>
  })

  const exit = await Effect.runPromise(
    Effect.scoped(program.pipe(Effect.provide(layer), Effect.exit)) as Effect.Effect<Exit.Exit<unknown, unknown>>,
  )
  if (Exit.isFailure(exit)) {
    const pretty = Cause.pretty(exit.cause)
    console.error(pretty)
    throw new Error(`capture: cassette ${scenario.cassette} failed\n${pretty}`)
  }

  const first = steps[0]
  if (first === undefined) throw new Error(`capture: cassette ${scenario.cassette} produced no steps`)
  if (built !== undefined) {
    for (const [index, step] of steps.entries()) {
      const expected = encodeModel(built, step.transport, auth)
      if (!Bun.deepEquals(expected, step.model))
        throw new Error(
          `capture: cassette ${scenario.cassette} step ${index} was not built from ${scenario.provider!.package}(${scenario.provider!.modelID})`,
        )
    }
  }
  return {
    cassette: scenario.cassette,
    protocol: scenario.protocol,
    routeId: (first.model.route as { id: string }).id,
    clockEpochMs: FIXED_EPOCH_MS,
    steps: steps.map(({ raw: _raw, ...step }) => step),
  }
}
