import { Cause, Effect, Exit, Layer, Option, Redacted, Schema } from "effect"
import { HttpClientRequest } from "effect/http"
import * as TestClock from "effect/testing/TestClock"
import { createLoader } from "./capture.js"

const tree = process.env.OPENCODE_UPSTREAM_TREE
if (!tree) throw new Error("realtime: OPENCODE_UPSTREAM_TREE is not set")

const load = createLoader(tree)

const { Realtime } = await load("packages/ai/src/experimental.ts")
const { AIErrorReason } = await load("packages/ai/src/schema/index.ts")
const { handlerLayer, runtimeLayer } = await load("packages/ai/test/lib/http.ts")

const CLOCK_EPOCH_MS = 1_760_000_000_000
const FIXTURE_BOUNDARY = "----OpenCodeFixtureBoundary"

type RecordedRequest = {
  method: string
  url: string
  headers: Record<string, string>
  body: string
  boundary?: string
}

type RealtimeInput = {
  engine: (typeof Realtime.engines)[number]
  model: string
  apiKey: string
  session?: Record<string, unknown>
  transport:
    | { type: "webrtc"; sdp: string; dcid?: number }
    | { type: "websocket" }
}

let active: Array<RecordedRequest> | undefined

const boundaryOf = (contentType: string | null) => {
  if (contentType === null) return undefined
  if (contentType.split(";", 1)[0]?.trim().toLowerCase() !== "multipart/form-data") return undefined
  const match = /boundary=(?:"([^"]+)"|([^;]+))/i.exec(contentType)
  return match?.[1] ?? match?.[2]?.trim()
}

const IGNORED_HEADERS = new Set(["b3", "traceparent"])

const record = (web: Request, body: string) => {
  if (active === undefined) throw new Error("realtime: a request arrived outside a case")
  const boundary = boundaryOf(web.headers.get("content-type"))
  const bodyBoundary = /^--([^\r\n]+)\r\n/.exec(body)?.[1]
  const headers: Record<string, string> = {}
  for (const [name, value] of web.headers.entries()) {
    if (IGNORED_HEADERS.has(name.toLowerCase())) continue
    headers[name] = boundary !== undefined && name.toLowerCase() === "content-type"
      ? value.replaceAll(boundary, FIXTURE_BOUNDARY)
      : value
  }
  const normalizedBody = bodyBoundary === undefined ? body : body.replaceAll(bodyBoundary, FIXTURE_BOUNDARY)
  active.push({
    method: web.method,
    url: web.url,
    headers,
    body: normalizedBody,
    ...(boundary === undefined ? {} : { boundary: FIXTURE_BOUNDARY }),
  })
}

const handler = (input: any) =>
  Effect.gen(function* () {
    const web = yield* HttpClientRequest.toWeb(input.request).pipe(Effect.orDie)
    record(web, input.text)
    if (web.url === "https://api.openai.com/v1/live/sessions")
      return input.respond(JSON.stringify({ session: { id: "s1" }, transport: { sdp: "v=0\r\no=live-answer\r\n" } }), {
        headers: { "content-type": "application/json" },
      })
    if (web.url === "https://api.openai.com/v1/realtime/calls?dcid=4")
      return input.respond("v=0\r\no=rtc-answer\r\n", { headers: { "content-type": "application/sdp" } })
    if (web.url === "https://api.openai.com/v1/realtime/client_secrets")
      return input.respond(JSON.stringify({ value: "ek_1", expires_at: 1_760_000_060 }))
    if (web.url === "https://api.x.ai/v1/realtime/client_secrets")
      return input.respond(JSON.stringify({ value: "ek_2", expires_at: 1_760_000_120 }))
    if (web.url === "https://generativelanguage.googleapis.com/v1beta/auth_tokens")
      return input.respond(JSON.stringify({ name: "auth_tokens/ek_3" }))
    return yield* Effect.die(`realtime: unexpected request ${web.method} ${web.url}`)
  })

const layer = Layer.mergeAll(
  TestClock.layer(),
  runtimeLayer(
    handlerLayer((input: any) => handler(input)),
  ),
)

const cases: ReadonlyArray<{ name: string; input: RealtimeInput }> = [
  {
    name: "openai-live/webrtc",
    input: {
      engine: Realtime.engines[0],
      model: "gpt-live-1",
      apiKey: "sk-openai",
      session: { instructions: "Be concise.", voice: "marin" },
      transport: { type: "webrtc", sdp: "v=0\r\no=offer\r\n", dcid: 4 },
    },
  },
  {
    name: "openai-realtime/webrtc",
    input: {
      engine: Realtime.engines[1],
      model: "gpt-realtime-2.1",
      apiKey: "sk-openai",
      session: { type: "realtime", instructions: "Brief." },
      transport: { type: "webrtc", sdp: "v=0\r\no=offer\r\n", dcid: 4 },
    },
  },
  {
    name: "openai-realtime/websocket",
    input: {
      engine: Realtime.engines[1],
      model: "gpt-realtime-2.1",
      apiKey: "sk-openai",
      session: { type: "realtime", instructions: "Brief." },
      transport: { type: "websocket" },
    },
  },
  {
    name: "xai-realtime/websocket",
    input: {
      engine: Realtime.engines[2],
      model: "grok-voice-3.0",
      apiKey: "xai-key",
      session: { voice: "ara" },
      transport: { type: "websocket" },
    },
  },
  {
    name: "gemini-live/websocket",
    input: {
      engine: Realtime.engines[3],
      model: "gemini-3.8-live",
      apiKey: "gemini-key",
      session: {
        generationConfig: { responseModalities: ["AUDIO"] },
        systemInstruction: { parts: [{ text: "Brief." }] },
      },
      transport: { type: "websocket" },
    },
  },
  {
    name: "openai-live/rejects-websocket",
    input: {
      engine: Realtime.engines[0],
      model: "gpt-live-1",
      apiKey: "k",
      transport: { type: "websocket" },
    },
  },
  {
    name: "xai-realtime/rejects-webrtc",
    input: {
      engine: Realtime.engines[2],
      model: "grok-voice-3.0",
      apiKey: "k",
      transport: { type: "webrtc", sdp: "v=0" },
    },
  },
  {
    name: "gemini-live/rejects-webrtc",
    input: {
      engine: Realtime.engines[3],
      model: "gemini-3.8-live",
      apiKey: "k",
      transport: { type: "webrtc", sdp: "v=0" },
    },
  },
]

const encodeError = (error: any) => {
  const reason = JSON.parse(JSON.stringify(Schema.encodeSync(AIErrorReason)(error.reason)))
  delete reason.cause
  return { reason, retryable: error.isRetryable }
}

const results = []
for (const item of cases) {
  active = []
  const exit = await Effect.runPromise(
    Effect.gen(function* () {
      yield* TestClock.setTime(CLOCK_EPOCH_MS)
      const connected: Effect.Effect<unknown, unknown, never> = Realtime.connect({
        engine: item.input.engine,
        model: item.input.model,
        apiKey: Redacted.make(item.input.apiKey),
        ...(item.input.session === undefined ? {} : { session: item.input.session }),
        transport: item.input.transport,
      })
      return yield* connected
    }).pipe(Effect.provide(layer), Effect.exit) as Effect.Effect<Exit.Exit<unknown, unknown>, never, never>,
  )
  const requests = active
  active = undefined
  const input = {
    engine: item.input.engine,
    model: item.input.model,
    apiKey: item.input.apiKey,
    ...(item.input.session === undefined ? {} : { session: item.input.session }),
    transport: item.input.transport,
  }
  if (Exit.isSuccess(exit)) results.push({ name: item.name, input, requests, result: exit.value })
  else {
    const error = Option.getOrUndefined(Cause.findErrorOption(exit.cause))
    if (error === undefined) throw new Error(`realtime: ${item.name} failed without an error`)
    results.push({ name: item.name, input, requests, error: encodeError(error) })
  }
}

process.stdout.write(JSON.stringify({ clockEpochMs: CLOCK_EPOCH_MS, cases: results }))
