import "./capture.js"
import { Cause, Context, Effect, Exit, Layer, Option } from "effect"
import { FetchHttpClient, HttpClientRequest, HttpClientResponse } from "effect/http"
import { createLoader, FIXTURE_BOUNDARY, FIXED_EPOCH_MS, multipartBoundary, normalizeMultipart } from "./capture.js"

const tree = process.env.OPENCODE_UPSTREAM_TREE
if (!tree) throw new Error("providers: OPENCODE_UPSTREAM_TREE is not set")

const load = createLoader(tree)

type Kind = "language" | "embedding" | "image" | "video" | "speech" | "transcription" | "decision"

type ProviderCase = {
  name: string
  specifier: string
  modelID: string
  settings: Record<string, unknown>
  env: Record<string, string>
  kind: Kind
  routeId: string
  descriptor: Record<string, unknown> | null
  prepared: CapturedRequest | null
  error: { message: string } | null
}

type CapturedRequest = {
  method: string
  url: string
  headers: Record<string, string>
  body: string
  boundary?: string
}

type CasePlan = {
  name: string
  settings: Record<string, unknown>
  env: Record<string, string>
}

const FIXTURE_AUDIO = "UklGRiQAAABXQVZFZm10IBAAAAABAAEAQB8AAEAfAAABAAgAZGF0YQAAAAA="

const manifest = await Bun.file(`${tree}/packages/ai/package.json`).json()
const specifiers: Array<string> = Object.keys(manifest.exports)
  .filter((key) => key.startsWith("./providers/"))
  .sort()
  .map((key) => `@opencode/ai/${key.slice(2)}`)

const { ai } = await load("packages/ai/src/effect.ts")
const { RequestExecutor } = await load("packages/ai/src/route.ts")
const { LLMClient } = await load("packages/ai/src/route.ts")
const { EmbeddingClient } = await load("packages/ai/src/embedding-client.ts")
const { DecisionClient } = await load("packages/ai/src/decision-client.ts")
const { ImageClient } = await load("packages/ai/src/image-client.ts")
const { VideoClient } = await load("packages/ai/src/video-client.ts")
const { SpeechClient } = await load("packages/ai/src/speech-client.ts")
const { TranscriptionClient } = await load("packages/ai/src/transcription-client.ts")
const { Media } = await load("packages/ai/src/media.ts")
const { toConfig } = await load("packages/ai/src/providers/settings.ts")

type Module = Record<string, unknown>
const modules = new Map<string, Module>()
for (const specifier of specifiers) {
  const key = specifier.replace(/^@opencode\/ai\//, "./")
  const module = await load(`packages/ai/${(manifest.exports[key] as string).slice(2)}`)
  modules.set(specifier, module as Module)
}

const records: Array<CapturedRequest & { bodyBytes: Uint8Array }> = []

const captureMiddleware = (request: unknown, _next: unknown) =>
  Effect.gen(function* () {
    const web = yield* HttpClientRequest.toWeb(request as never)
    const bytes = new Uint8Array(yield* Effect.promise(() => web.arrayBuffer()))
    records.push({
      method: web.method,
      url: web.url,
      headers: Object.fromEntries(web.headers.entries()),
      body: "",
      bodyBytes: bytes,
    })
    return HttpClientResponse.fromWeb(
      request as never,
      new Response("{}", { status: 599, headers: { "content-type": "application/json" } }),
    )
  })

const executorLayer = RequestExecutor.layer.pipe(Layer.provide(FetchHttpClient.layer))
const capturing = RequestExecutor.middleware(captureMiddleware, executorLayer)
const layer = Layer.mergeAll(
  LLMClient.layer,
  EmbeddingClient.layer,
  DecisionClient.layer,
  ImageClient.layer,
  VideoClient.layer,
  SpeechClient.layer,
  TranscriptionClient.layer,
).pipe(Layer.provide(capturing))
const context = (await Effect.runPromise(
  Effect.scoped(Layer.build(layer) as Effect.Effect<unknown, never, never>),
)) as Context.Context<never>

const LANGUAGE_IDS: Record<string, string> = {
  anthropic: "claude-haiku-4-5-20251001",
  "anthropic-compatible": "claude-haiku-4-5",
  "amazon-bedrock": "us.amazon.nova-micro-v1:0",
  "amazon-bedrock-mantle": "openai.gpt-oss-120b",
  alibaba: "qwen3-max",
  azure: "gpt-5.6-luna",
  baseten: "fixture-model",
  cerebras: "gpt-oss-120b",
  "cloudflare-ai-gateway": "openai/gpt-5-nano",
  "cloudflare-workers-ai": "@cf/meta/llama-3.1-8b-instruct",
  cohere: "command-a-03-2025",
  deepinfra: "meta-llama/Llama-3.3-70B-Instruct-Turbo",
  deepseek: "deepseek-chat",
  digitalocean: "gpt-5",
  fireworks: "accounts/fireworks/models/llama-v3p1-70b-instruct",
  google: "gemini-3.5-flash",
  "google-vertex": "gemini-3.5-flash",
  "google-vertex-messages": "claude-haiku-4-5",
  groq: "llama-3.3-70b-versatile",
  meta: "llama-4-maverick",
  minimax: "MiniMax-M3",
  mistral: "mistral-large-latest",
  moonshot: "kimi-k2.5",
  openai: "gpt-5.5",
  "openai-compatible": "fixture-model",
  "openai-compatible-responses": "fixture-model",
  opencode: "fixture-model",
  openrouter: "openai/gpt-5-nano",
  togetherai: "meta-llama/Llama-3.3-70B-Instruct-Turbo",
  "typesafe-ai": "fixture-model",
  venice: "openai-gpt-54-mini",
  "vercel-ai-gateway": "anthropic/claude-sonnet-4.6",
  xai: "grok-4",
  zai: "glm-5",
  "zai-coding-plan": "glm-5",
}

const EMBEDDING_IDS: Record<string, string> = {
  alibaba: "text-embedding-v4",
  cohere: "embed-english-v3.0",
  google: "gemini-embedding-001",
  "google-vertex": "text-embedding-005",
  openai: "text-embedding-3-small",
  azure: "text-embedding-3-small",
}

const IMAGE_IDS: Record<string, string> = {
  alibaba: "wan2.5-t2i-preview",
  "cloudflare-workers-ai": "@cf/black-forest-labs/flux-1-schnell",
  google: "imagen-3.0-generate-002",
  "google-vertex": "imagen-3.0-generate-002",
  meta: "meta-image",
  minimax: "MiniMax-Image-01",
  openai: "gpt-image-1",
  openrouter: "openai/gpt-image-1",
  "vercel-ai-gateway": "openai/gpt-image-1",
  xai: "grok-2-image",
  zai: "glm-image",
}

const VIDEO_IDS: Record<string, string> = {
  byteplus: "seedance-1-0-pro",
  google: "veo-3.1",
  minimax: "MiniMax-Hailuo-02",
  openrouter: "openai/sora-2",
  volcengine: "seedance-1-0-pro",
  xai: "grok-2-video",
}

const SPEECH_IDS: Record<string, string> = {
  cartesia: "sonic-2",
  deepgram: "aura-2-thalia-en",
  elevenlabs: "eleven_multilingual_v2",
  google: "gemini-2.5-flash-preview-tts",
  openai: "tts-1",
}

const TRANSCRIPTION_IDS: Record<string, string> = {
  assemblyai: "best",
  deepgram: "nova-2",
  elevenlabs: "scribe_v1",
  google: "gemini-2.5-flash",
  openai: "whisper-1",
}

const providerPath = (value: string) => value.replace(/^@opencode\/ai\/providers\//, "")

const languageId = (path: string) => {
  const root = path.split("/")[0]!
  if (path === "google-vertex/chat" || path === "google-vertex/interactions") return "gemini-3.5-flash"
  if (path === "google-vertex/mistral") return "mistral-large-latest"
  return LANGUAGE_IDS[root] ?? "fixture-model"
}

function modelIdFor(path: string, kind: Kind) {
  const root = path.split("/")[0]!
  if (kind === "embedding") return EMBEDDING_IDS[root] ?? "fixture-embedding"
  if (kind === "image") return IMAGE_IDS[root] ?? "fixture-image"
  if (kind === "video") return VIDEO_IDS[root] ?? "fixture-video"
  if (kind === "speech") return SPEECH_IDS[root] ?? "fixture-speech"
  if (kind === "transcription") return TRANSCRIPTION_IDS[root] ?? "fixture-transcription"
  if (kind === "decision") return "fixture-model"
  return languageId(path)
}

const OAUTH_VERTEX = new Set([
  "google-vertex-messages",
  "google-vertex/chat",
  "google-vertex/messages",
  "google-vertex/mistral",
  "google-vertex/responses",
])

const oauthSettings = () => ({ accessToken: "fixture-token", project: "fixture-project", location: "us-central1" })

function customBaseURL(path: string) {
  const root = path.split("/")[0]!
  if (root === "cloudflare-ai-gateway" || root === "cloudflare-workers-ai")
    return "https://fixture.example.test/client/v4/accounts/fixture/ai/v1"
  return "https://fixture.example.test/v1"
}

function baseSettings(path: string): Record<string, unknown> {
  const root = path.split("/")[0]!
  if (root === "alibaba") return { apiKey: "fixture", region: "us-east-1" }
  if (root === "anthropic-compatible") return { apiKey: "fixture", baseURL: "https://fixture.example.test/anthropic/v1" }
  if (root === "azure") return { apiKey: "fixture", resourceName: "fixture-resource" }
  if (root === "cloudflare-ai-gateway") return { apiKey: "fixture", accountId: "fixture-account" }
  if (root === "cloudflare-workers-ai") return { apiKey: "fixture", accountId: "fixture-account" }
  if (OAUTH_VERTEX.has(path)) return oauthSettings()
  if (root === "openai-compatible" || root === "openai-compatible-responses")
    return { apiKey: "fixture", baseURL: "https://fixture.example.test/v1" }
  return { apiKey: "fixture" }
}

function extras(path: string): Array<CasePlan> {
  const root = path.split("/")[0]!
  const cases: Array<CasePlan> = []
  if (root === "alibaba") {
    cases.push({ name: "beijing-region", settings: { apiKey: "fixture", region: "cn-beijing" }, env: {} })
    cases.push({
      name: "workspace",
      settings: { apiKey: "fixture", region: "us-east-1", workspaceID: "fixture-workspace" },
      env: {},
    })
  }
  if (root === "amazon-bedrock") {
    cases.push({
      name: "sigv4",
      settings: {
        credentials: { region: "us-west-2", accessKeyId: "AKIAIOSFODNN7EXAMPLE", secretAccessKey: "fixture-secret" },
      },
      env: {},
    })
    cases.push({ name: "region", settings: { apiKey: "fixture", region: "eu-west-1" }, env: {} })
  }
  if (root === "azure") {
    cases.push({
      name: "deployment-urls",
      settings: {
        apiKey: "fixture",
        resourceName: "fixture-resource",
        useDeploymentBasedUrls: true,
        apiVersion: "2025-01-01",
      },
      env: {},
    })
  }
  if (root === "anthropic") {
    cases.push({ name: "auth-token", settings: { authToken: "fixture-token" }, env: {} })
  }
  if (root === "anthropic-compatible") {
    cases.push({
      name: "provider-auth-token",
      settings: {
        authToken: "fixture-token",
        baseURL: "https://fixture.example.test/anthropic/v1",
        provider: "fixture-provider",
      },
      env: {},
    })
  }
  if (root === "cloudflare-ai-gateway") {
    cases.push({
      name: "gateway-key",
      settings: {
        accountId: "fixture-account",
        gatewayId: "fixture-gateway",
        gatewayApiKey: "fixture-gateway-key",
      },
      env: {},
    })
    cases.push({
      name: "gateway-options",
      settings: {
        accountId: "fixture-account",
        gatewayId: "fixture-gateway",
        gatewayApiKey: "fixture-gateway-key",
        metadata: { team: "fixture" },
        cacheTtl: 60,
        cacheKey: "fixture-cache",
        skipCache: true,
        collectLog: true,
      },
      env: {},
    })
  }
  if (root === "cloudflare-workers-ai") {
    cases.push({
      name: "base-url",
      settings: {
        apiKey: "fixture",
        baseURL: "https://fixture.example.test/client/v4/accounts/fixture/ai/v1",
      },
      env: {},
    })
  }
  if (root === "google-vertex" || root === "google-vertex-messages") {
    cases.push({ name: "oauth", settings: oauthSettings(), env: {} })
  }
  if (root === "openai") {
    cases.push({
      name: "organization-project",
      settings: { apiKey: "fixture", organization: "fixture-org", project: "fixture-project" },
      env: {},
    })
  }
  if (path === "openai") {
    cases.push({
      name: "query-params",
      settings: { apiKey: "fixture", queryParams: { "api-version": "x" } },
      env: {},
    })
  }
  if (root === "openai-compatible" || root === "openai-compatible-responses") {
    cases.push({
      name: "provider",
      settings: { apiKey: "fixture", baseURL: "https://fixture.example.test/v1", provider: "fixture-provider" },
      env: {},
    })
  }
  if (root === "venice") {
    cases.push({
      name: "query-params",
      settings: { apiKey: "fixture", queryParams: { trace: "fixture" } },
      env: {},
    })
  }
  return cases
}

function errors(path: string): Array<CasePlan> {
  const root = path.split("/")[0]!
  const cases: Array<CasePlan> = []
  if (root === "alibaba") cases.push({ name: "missing-region", settings: { apiKey: "fixture" }, env: {} })
  if (root === "anthropic")
    cases.push({ name: "api-key-and-auth-token", settings: { apiKey: "fixture", authToken: "fixture" }, env: {} })
  if (root === "anthropic-compatible")
    cases.push({ name: "missing-base-url", settings: { apiKey: "fixture" }, env: {} })
  if (root === "azure") cases.push({ name: "missing-resource", settings: { apiKey: "fixture" }, env: {} })
  if (root === "amazon-bedrock")
    cases.push({ name: "sigv4-with-api-key", settings: { apiKey: "fixture", auth: "sigv4" }, env: {} })
  if (root === "amazon-bedrock-mantle")
    cases.push({ name: "sigv4-with-api-key", settings: { apiKey: "fixture", auth: "sigv4" }, env: {} })
  if (root === "cloudflare-ai-gateway")
    cases.push({ name: "missing-account", settings: { apiKey: "fixture" }, env: {} })
  if (root === "cloudflare-workers-ai")
    cases.push({ name: "missing-account", settings: { apiKey: "fixture" }, env: {} })
  if (OAUTH_VERTEX.has(path)) cases.push({ name: "api-key", settings: { apiKey: "fixture" }, env: {} })
  if (root === "google-vertex" && !OAUTH_VERTEX.has(path))
    cases.push({
      name: "api-key-and-access-token",
      settings: {
        apiKey: "fixture",
        accessToken: "fixture-token",
        project: "fixture-project",
        location: "us-central1",
      },
      env: {},
    })
  return cases
}

function buildModel(module: Module, modelID: string, settings: Record<string, unknown>) {
  if (typeof module.model === "function") return module.model(modelID, settings)
  if (typeof module.decisionModel === "function") return module.decisionModel(modelID, settings)
  if (typeof module.videoModel === "function") return module.videoModel(modelID, settings)
  if (typeof module.configure === "function") {
    const config = (toConfig as Function)(settings)
    if (typeof module.speech === "function") return (module.configure as Function)(config).speech(modelID)
    if (typeof module.transcription === "function") return (module.configure as Function)(config).transcription(modelID)
  }
  if (typeof module.speech === "function") return module.speech(modelID)
  if (typeof module.transcription === "function") return module.transcription(modelID)
  throw new Error("no provider entrypoint export")
}

const KIND_NAMES: Record<string, Kind> = {
  LanguageModel: "language",
  EmbeddingModel: "embedding",
  DecisionModel: "decision",
  ImageModel: "image",
  VideoModel: "video",
  SpeechModel: "speech",
  TranscriptionModel: "transcription",
}

function descriptorFor(kind: Kind, model: Record<string, any>) {
  if (kind !== "language") return { provider: model.provider, routeId: model.route.id }
  const path = model.route.endpoint.path
  return {
    id: model.id,
    provider: model.provider,
    route: {
      id: model.route.id,
      protocol: model.route.protocol,
      providerMetadataKey: model.route.providerMetadataKey,
    },
    endpoint: {
      baseURL: model.route.endpoint.baseURL,
      path: typeof path === "string" ? path : null,
      query: model.route.endpoint.query,
    },
    compatibility: model.compatibility,
    capabilities: model.capabilities,
  }
}

const SPEECH_VOICE: Record<string, string> = {
  cartesia: "f786b574-daa5-4673-aa0c-cbe3e8534c02",
  elevenlabs: "JBFqnCBsd6RMkjVDRZzb",
}

function operation(kind: Kind, model: unknown, path: string) {
  if (kind === "language") return ai.generate({ model, prompt: "Hi" })
  if (kind === "embedding") return ai.embedding.generate({ model, values: ["fixture"] })
  if (kind === "decision")
    return ai.decision.generate({
      model,
      input: [{ type: "text", text: "fixture" }],
      questions: [{ type: "predicate", name: "ok", instructions: "Say whether the text is the word fixture" }],
    })
  if (kind === "image") return ai.image.generate({ model, prompt: "fixture" })
  if (kind === "video") return ai.video.start({ model, prompt: "fixture" })
  if (kind === "speech") {
    const voice = SPEECH_VOICE[path.split("/")[0]!]
    return ai.experimental.speech.generate({ model, text: "fixture", ...(voice === undefined ? {} : { voice }) })
  }
  if (kind === "transcription")
    return ai.experimental.transcription.generate({
      model,
      audio: Media.base64(FIXTURE_AUDIO, "audio/wav"),
    })
  throw new Error(`unsupported kind ${kind}`)
}

function capturePrepared(record: (typeof records)[number]): CapturedRequest {
  const contentType = record.headers["content-type"]
  const boundary = multipartBoundary(contentType)
  if (boundary === undefined)
    return {
      method: record.method,
      url: record.url,
      headers: record.headers,
      body: Buffer.from(record.bodyBytes).toString("base64"),
    }
  const normalized = normalizeMultipart(record.headers, Buffer.from(record.bodyBytes).toString("latin1"))
  return {
    method: record.method,
    url: record.url,
    headers: normalized.headers,
    body: Buffer.from(normalized.body, "latin1").toString("base64"),
    boundary: FIXTURE_BOUNDARY,
  }
}

async function run(plans: Array<CasePlan>, kind: Kind, modelID: string, module: Module, path: string) {
  const results: Array<{ plan: CasePlan; prepared: CapturedRequest | null; error: { message: string } | null }> = []
  for (const plan of plans) {
    const saved = new Map<string, string | undefined>()
    for (const [name, value] of Object.entries(plan.env)) {
      saved.set(name, process.env[name])
      process.env[name] = value
    }
    records.length = 0
    let model: Record<string, any>
    try {
      model = buildModel(module, modelID, plan.settings)
    } catch (cause) {
      results.push({ plan, prepared: null, error: { message: errorMessage(cause) } })
      restore(saved)
      continue
    }
    const exit = await Effect.runPromise(Effect.exit(operation(kind, model, path).pipe(Effect.provide(context))))
    restore(saved)
    const prepared = records.length === 0 ? null : capturePrepared(records[0]!)
    const error = prepared === null ? rejection(exit) : null
    results.push({ plan, prepared, error })
  }
  return results
}

function rejection(exit: Exit.Exit<unknown, unknown>) {
  if (Exit.isSuccess(exit)) return { message: "operation produced no request" }
  const cause = Option.getOrUndefined(Cause.findErrorOption(exit.cause))
  if (cause === undefined) throw new Error(Cause.pretty(exit.cause))
  return { message: errorMessage(cause) }
}

function errorMessage(cause: unknown) {
  if (cause instanceof Error) return cause.message
  return String(cause)
}

function restore(saved: Map<string, string | undefined>) {
  for (const [name, value] of saved) {
    if (value === undefined) delete process.env[name]
    else process.env[name] = value
  }
}

const cases: Array<ProviderCase> = []
for (const specifier of specifiers) {
  const path = providerPath(specifier)
  const module = modules.get(specifier)!
  const probeKind: Kind =
    module.model === undefined && typeof module.speech === "function"
      ? "speech"
      : module.model === undefined && typeof module.transcription === "function"
        ? "transcription"
        : module.model === undefined && typeof module.videoModel === "function"
          ? "video"
          : "language"
  const modelID = modelIdFor(path, probeKind)
  let kind: Kind
  try {
    const model = buildModel(module, modelID, baseSettings(path))
    kind = KIND_NAMES[model.constructor.name] ?? probeKind
  } catch (cause) {
    throw new Error(`${specifier}: default case failed to build: ${errorMessage(cause)}`)
  }
  const resolvedID = modelIdFor(path, kind)
  const effectiveID = resolvedID === modelID ? modelID : resolvedID

  const base = baseSettings(path)
  const plans: Array<CasePlan> = [{ name: "default", settings: base, env: {} }]
  plans.push({
    name: "custom",
    settings: {
      ...base,
      baseURL: customBaseURL(path),
      headers: { "x-fixture": "one" },
      body: { fixture: "one" },
    },
    env: {},
  })
  plans.push(...extras(path))
  plans.push(...errors(path))

  const results = await run(plans, kind, effectiveID, module, path)
  for (const result of results) {
    let routeId = ""
    let descriptor: Record<string, unknown> | null = null
    if (result.prepared !== null) {
      const model = buildModel(module, effectiveID, result.plan.settings)
      routeId = model.route.id
      descriptor = descriptorFor(kind, model)
    }
    cases.push({
      name: `${path}:${result.plan.name}`,
      specifier,
      modelID: effectiveID,
      settings: result.plan.settings,
      env: result.plan.env,
      kind,
      routeId,
      descriptor,
      prepared: result.prepared,
      error: result.error,
    })
  }
}

for (const specifier of specifiers) {
  if (!cases.some((entry) => entry.specifier === specifier)) throw new Error(`no case for ${specifier}`)
}

process.stdout.write(
  JSON.stringify({
    clockEpochMs: FIXED_EPOCH_MS,
    cases,
  }),
)
