import "./capture.js"
import { Effect, Schema, Stream } from "effect"
import { HttpClientRequest } from "effect/http"
import { capture, createLoader, multipartFixtures, type Scenario } from "./capture.js"

const tree = process.env.OPENCODE_UPSTREAM_TREE
if (!tree) throw new Error("scenarios: OPENCODE_UPSTREAM_TREE is not set")

const load = createLoader(tree)

const { ai, CacheHint, LLMEvent, Media, Message, SystemPart, ToolCallPart, ToolChoice, ToolDefinition, ToolRuntime } =
  await load("packages/ai/src/effect.ts")
const { LLMClient } = await load("packages/ai/src/route.ts")
const {
  Alibaba,
  Anthropic,
  AnthropicCompatible,
  AmazonBedrock,
  AmazonBedrockMantle,
  Azure,
  Cerebras,
  CloudflareAIGateway,
  CloudflareWorkersAI,
  Cohere,
  DeepInfra,
  DigitalOcean,
  Google,
  GoogleVertex,
  Groq,
  Meta,
  MiniMax,
  Mistral,
  Moonshot,
  OpenAI,
  OpenAICompatible,
  OpenRouter,
  TogetherAI,
  VercelAIGateway,
  Venice,
  XAI,
  ZAI,
} = await load("packages/ai/src/providers.ts")
const { toDefinitions, tool } = await load("packages/ai/src/tool.ts")
const {
  goldenWeatherToolLoopRequest,
  LARGE_CACHEABLE_SYSTEM,
  runGoldenScenario,
  runWeatherToolLoop,
  weatherRuntimeTool,
  weatherTool,
  weatherToolLoopRequest,
} = await load("packages/ai/test/recorded-scenarios.ts")
const { runTools } = await load("packages/ai/test/lib/tool-runtime.ts")

const anthropic = Anthropic.configure({ apiKey: "fixture" })
const anthropicHaiku = anthropic.model("claude-haiku-4-5-20251001")
const anthropicOpus = anthropic.model("claude-opus-4-7")
const minimax = AnthropicCompatible.configure({
  apiKey: "fixture",
  baseURL: "https://api.minimax.io/anthropic/v1",
  provider: "minimax",
}).model("MiniMax-M3")
const openai = OpenAI.configure({ apiKey: "fixture" })
const openaiResponses = openai.responses("gpt-5.5")
const google = Google.configure({ apiKey: "fixture" })
const geminiFlash = google.model("gemini-2.5-flash")
const pdfGemini = google.model("gemini-3.5-flash")
const together = TogetherAI.configure({ apiKey: "fixture" }).model("meta-llama/Llama-3.3-70B-Instruct-Turbo")
const cerebras = Cerebras.configure({ apiKey: "fixture" }).model("gpt-oss-120b")
const deepinfra = DeepInfra.configure({ apiKey: "fixture" }).model("meta-llama/Llama-3.3-70B-Instruct-Turbo")
const vercel = OpenAICompatible.configure({
  provider: "vercel-ai-gateway",
  baseURL: "https://ai-gateway.vercel.sh/v1",
  apiKey: "fixture",
  http: { body: { reasoning: { enabled: true, max_tokens: 1024 } } },
}).model("anthropic/claude-sonnet-4.6")
const venice = Venice.configure({ apiKey: "fixture" }).responses("openai-gpt-54-mini")
const geminiOpenAIChat = OpenAICompatible.configure({
  provider: "google",
  baseURL: "https://generativelanguage.googleapis.com/v1beta/openai",
  apiKey: "fixture",
}).model("gemini-3.8-flash")
const bedrockConversation = AmazonBedrock.configure({
  credentials: { region: "us-east-1", accessKeyId: "fixture", secretAccessKey: "fixture" },
}).model("us.amazon.nova-micro-v1:0")
const bedrockCache = AmazonBedrock.configure({ apiKey: "fixture", region: "us-east-1" }).model(
  "us.anthropic.claude-haiku-4-5-20251001-v1:0",
)
const bedrockPdf = AmazonBedrock.configure({ apiKey: "fixture", region: "us-east-1" }).model(
  "us.anthropic.claude-haiku-4-5-20251001-v1:0",
)

const azureResourceName = "aiden-azury-group"
const azureFoundryBaseURL = `https://${azureResourceName}.services.ai.azure.com/models`
const azure = Azure.configure({ resourceName: azureResourceName, apiKey: "fixture" })
const azureChat = azure.chat("gpt-5.6-luna")
const azureResponses = azure.responses("gpt-5.6-luna")
const azureMessages = azure.messages("claude-haiku-4-5")
const azureFoundryChat = Azure.configure({ baseURL: azureFoundryBaseURL, apiKey: "fixture" }).chat("Kimi-K2.5")
const azureLookupWeather = ToolDefinition.make({
  name: "lookup_weather",
  description: "Look up the current weather for a city",
  inputSchema: { type: "object", properties: { city: { type: "string" } }, required: ["city"] },
})
const mantleOptions = { apiKey: "fixture", region: "us-east-1" }
const mantleResponses = AmazonBedrockMantle.configure(mantleOptions).responses("openai.gpt-oss-120b")
const mantleMessages = (modelID: string) => AmazonBedrockMantle.configure(mantleOptions).messages(modelID)
const mantleWeather = ToolDefinition.make({
  name: "get_weather",
  description: "Get the current weather in a city",
  inputSchema: {
    type: "object",
    properties: { city: { type: "string", enum: ["Paris"] } },
    required: ["city"],
    additionalProperties: false,
  },
})

const cloudflareAccountId = "fixture-account"
const cloudflareAIGateway = CloudflareAIGateway.configure({
  accountId: cloudflareAccountId,
  gatewayApiKey: "fixture",
})
const cloudflareWorkers = CloudflareWorkersAI.configure({
  accountId: cloudflareAccountId,
  apiKey: "fixture",
})
const cloudflareAIGatewayWorkers = cloudflareAIGateway.model("workers-ai/@cf/meta/llama-3.1-8b-instruct")
const cloudflareAIGatewayWorkersTools = cloudflareAIGateway.model("workers-ai/@cf/openai/gpt-oss-20b")
const cloudflareAIGatewayClaude = cloudflareAIGateway.model("anthropic/claude-haiku-4.5")
const cloudflareAIGatewayOpenAI = cloudflareAIGateway.model("openai/gpt-5-nano")
const cloudflareWorkersAI = cloudflareWorkers.model("@cf/meta/llama-3.1-8b-instruct")
const cloudflareWorkersAITools = cloudflareWorkers.model("@cf/openai/gpt-oss-20b")
const redactCloudflareURL = (url: string) =>
  url
    .replace(/\/client\/v4\/accounts\/[^/]+\/ai\/v1\//, "/client/v4/accounts/{account}/ai/v1/")
    .replace(/\/v1\/[^/]+\/[^/]+\/(anthropic|openai)\//, "/v1/{account}/{gateway}/$1/")
const cloudflareRedact = { url: redactCloudflareURL }
const cloudflareClaudeRedact = { url: redactCloudflareURL, allowRequestHeaders: ["anthropic-version"] }
const vercelGateway = VercelAIGateway.configure({ apiKey: "fixture" })
const vercelGatewayChat = VercelAIGateway.configure({ apiKey: "fixture" })
const vercelGatewaySettings = { apiKey: "fixture" }
const cohere = Cohere.configure({ apiKey: "fixture" })
const googleInteractions = Google.configure({ apiKey: "fixture" }).interactions("gemini-3.8-flash")
const googleVertex = GoogleVertex.configure({ apiKey: "fixture" }).model("gemini-3.5-flash")

const PDF =
  "JVBERi0xLjQKMSAwIG9iago8PCAvVHlwZSAvQ2F0YWxvZyAvUGFnZXMgMiAwIFIgPj4KZW5kb2JqCjIgMCBvYmoKPDwgL1R5cGUgL1BhZ2VzIC9LaWRzIFszIDAgUl0gL0NvdW50IDEgPj4KZW5kb2JqCjMgMCBvYmoKPDwgL1R5cGUgL1BhZ2UgL1BhcmVudCAyIDAgUiAvTWVkaWFCb3ggWzAgMCA2MTIgNzkyXSAvUmVzb3VyY2VzIDw8IC9Gb250IDw8IC9GMSA1IDAgUiA+PiA+PiAvQ29udGVudHMgNCAwIFIgPj4KZW5kb2JqCjQgMCBvYmoKPDwgL0xlbmd0aCA3NSA+PgpzdHJlYW0KQlQKL0YxIDE4IFRmCjcyIDcyMCBUZAooUERGIGNhc3NldHRlIHZlcmlmaWNhdGlvbiBjb2RlOiBPUkNISUQtNzM5MSkgVGoKRVQKZW5kc3RyZWFtCmVuZG9iago1IDAgb2JqCjw8IC9UeXBlIC9Gb250IC9TdWJ0eXBlIC9UeXBlMSAvQmFzZUZvbnQgL0hlbHZldGljYSA+PgplbmRvYmoKeHJlZgowIDYKMDAwMDAwMDAwMCA2NTUzNSBmIAowMDAwMDAwMDA5IDAwMDAwIG4gCjAwMDAwMDAwNTggMDAwMDAgbiAKMDAwMDAwMDExNSAwMDAwMCBuIAowMDAwMDAwMjQxIDAwMDAwIG4gCjAwMDAwMDAzNjUgMDAwMDAgbiAKdHJhaWxlcgo8PCAvU2l6ZSA2IC9Sb290IDEgMCBSID4+CnN0YXJ0eHJlZgo0MzUKJSVFT0YK"
const pdfPrompt = "Return only the verification code from the PDF."
const readPdf = ToolDefinition.make({
  name: "read_pdf",
  description: "Read the attached PDF.",
  inputSchema: { type: "object", properties: {}, additionalProperties: false },
})
const readPdfRuntime = tool({
  name: "read_pdf",
  description: readPdf.description,
  input: Schema.Struct({ path: Schema.String }),
  success: Schema.String,
  execute: () => Effect.succeed("PDF read successfully"),
  toModelOutput: () => [
    { type: "text", text: "PDF read successfully" },
    { type: "file", uri: `data:application/pdf;base64,${PDF}`, mime: "application/pdf", name: "verification.pdf" },
  ],
})

const anthropicCacheRequest = ai.request({
  id: "recorded_anthropic_cache",
  model: anthropicHaiku,
  system: [{ type: "text", text: LARGE_CACHEABLE_SYSTEM, cache: new CacheHint({ type: "ephemeral" }) }],
  prompt: "Say hi.",
  cache: "none",
  maxTokens: 16,
  temperature: 0,
})
const lookup = ToolDefinition.make({
  name: "lookup",
  description: "Look up a fixture value.",
  inputSchema: {
    type: "object",
    properties: { index: { type: "number" } },
    required: ["index"],
    additionalProperties: false,
  },
})
const longToolTurn = [
  Message.user("Run the fixture lookups."),
  ...Array.from({ length: 11 }, (_, index) => {
    const id = `lookup_${index}`
    return [
      Message.assistant(ToolCallPart.make({ id, name: lookup.name, input: { index } })),
      Message.tool({ id, name: lookup.name, result: `Fixture result ${index}. `.repeat(80) }),
    ]
  }).flat(),
]
const longToolTurnRequest = ai.request({
  id: "recorded_anthropic_cache_long_tool_turn",
  model: anthropicHaiku,
  system: LARGE_CACHEABLE_SYSTEM,
  messages: longToolTurn,
  tools: [lookup],
  maxTokens: 16,
  temperature: 0,
})

const openaiCacheRequest = ai.request({
  id: "recorded_openai_responses_cache",
  model: openai.responses("gpt-4.1-mini"),
  system: LARGE_CACHEABLE_SYSTEM,
  prompt: "Say hi.",
  maxTokens: 16,
  temperature: 0,
  promptCacheKey: "recorded-cache-test",
})
const openaiFrontierRequest = ai.request({
  id: "recorded_openai_responses_cache_gpt_6_2",
  model: openai.responses("gpt-6.2-sol"),
  system: LARGE_CACHEABLE_SYSTEM,
  prompt: "Say hi.",
  maxTokens: 64,
  promptCacheKey: "recorded-cache-test-gpt-6-2",
  providerOptions: { reasoningEffort: "low" },
})

const geminiCacheRequest = ai.request({
  id: "recorded_gemini_cache",
  model: geminiFlash,
  system: LARGE_CACHEABLE_SYSTEM,
  prompt: "Say hi.",
  maxTokens: 16,
  temperature: 0,
})

const bedrockCacheRequest = ai.request({
  id: "recorded_bedrock_cache",
  model: bedrockCache,
  system: [{ type: "text", text: LARGE_CACHEABLE_SYSTEM, cache: new CacheHint({ type: "ephemeral" }) }],
  prompt: "Say hi.",
  cache: "none",
  maxTokens: 16,
  temperature: 0,
})

const bedrockOptions = (id: string, settings: Record<string, unknown>) =>
  AmazonBedrock.model(id, { apiKey: "fixture", region: "us-east-1", ...settings })

const bedrockThinking = (blockBinding?: { readonly prefix_mismatch_behavior: string }) =>
  AmazonBedrock.model("global.anthropic.claude-opus-5-5", {
    apiKey: "fixture",
    region: "us-east-1",
    body: {
      additionalModelRequestFields: {
        thinking: { type: "adaptive", display: "summarized", ...(blockBinding ? { block_binding: blockBinding } : {}) },
        output_config: { effort: "high" },
      },
    },
  })

const freeformRun = toDefinitions([
  tool({
    name: "run",
    description: "Run a Python snippet.",
    input: Schema.Struct({ code: Schema.String }),
    success: Schema.String,
    freeform: { type: "text" },
  }),
])

const chatVariant = (scenario: Scenario) => ({ group: "chat-variants", scenario })

const fixtureSettings = { apiKey: "fixture" }
const integerPrompt = "What is 173 multiplied by 219? Reply with only the final integer."
const weatherPrompt =
  "Use get_weather to look up the current weather in Paris. After receiving the result, report the weather in one short sentence."
const minimaxWeatherPrompt =
  "Look up the current weather in Paris using get_weather before answering. After receiving the result, report the weather in one short sentence."
const locationSchema = {
  type: "object",
  properties: { city: { type: "string", enum: ["Paris"] } },
  required: ["city"],
  additionalProperties: false,
}
const chatWeather = ToolDefinition.make({
  name: "get_weather",
  description: "Get the current weather in a city",
  inputSchema: locationSchema,
})
const legacyChatWeather = ToolDefinition.make({
  name: "get_weather",
  description: "Get weather in a city",
  inputSchema: {
    type: "object",
    properties: { city: { type: "string", enum: ["Paris"] } },
    required: ["city"],
  },
})
const restroomImage = () =>
  Effect.promise(() => Bun.file(`${tree}/packages/ai/test/fixtures/media/restroom.png`).bytes())

const configureOptions = (settings: Record<string, unknown>) => {
  const { apiKey, ...providerOptions } = settings
  return Object.keys(providerOptions).length === 0 ? { apiKey } : { apiKey, providerOptions }
}

const zaiModel = (modelID: string, settings: Record<string, unknown>) => ZAI.model(modelID, settings)
const moonshotChatModel = (modelID: string, settings: Record<string, unknown>) => Moonshot.model(modelID, settings)
const moonshotMessagesModel = (modelID: string, settings: Record<string, unknown>) =>
  Moonshot.configure(configureOptions(settings)).messages(modelID)
const moonshotResponsesModel = (modelID: string, settings: Record<string, unknown>) =>
  Moonshot.configure(configureOptions(settings)).responses(modelID)
const minimaxMessagesModel = (modelID: string, settings: Record<string, unknown>) => MiniMax.model(modelID, settings)
const minimaxChatModel = (modelID: string, settings: Record<string, unknown>) =>
  MiniMax.configure(configureOptions(settings)).chat(modelID)
const minimaxResponsesModel = (modelID: string, settings: Record<string, unknown>) =>
  MiniMax.configure(configureOptions(settings)).responses(modelID)

const moonshotApi = {
  chat: { protocol: "openai-chat", package: "@opencode/ai/providers/moonshot", model: moonshotChatModel },
  messages: {
    protocol: "anthropic-messages",
    package: "@opencode/ai/providers/moonshot/messages",
    model: moonshotMessagesModel,
  },
  responses: {
    protocol: "open-responses",
    package: "@opencode/ai/providers/moonshot/responses",
    model: moonshotResponsesModel,
  },
} as const

const runThreeStepWeatherLoop = (request: any) =>
  Effect.gen(function* () {
    const first = yield* LLMClient.generate(request)
    const continuation = {
      ...request,
      messages: [
        ...request.messages,
        first.message,
        ...first.toolCalls.map((call: any) =>
          Message.tool({ id: call.id, name: call.name, result: { condition: "sunny", temperature: "18C" } }),
        ),
      ],
    }
    const second = yield* LLMClient.generate(continuation)
    yield* LLMClient.generate({
      ...continuation,
      messages: [
        ...continuation.messages,
        second.message,
        Message.user("What temperature did the tool report? Reply with only the temperature."),
      ],
    })
  })

const runChatToolLoop = (request: any) =>
  Effect.gen(function* () {
    const first = yield* LLMClient.generate(request)
    yield* LLMClient.generate({
      ...request,
      toolChoice: ToolChoice.make("none"),
      messages: [
        ...request.messages,
        first.message,
        ...first.toolCalls.map((call: any) =>
          Message.tool({ id: call.id, name: call.name, result: { condition: "sunny", temperature: "18C" } }),
        ),
      ],
    })
  })

const moonshotEffortScenarios = (api: {
  readonly name: "chat" | "messages" | "responses"
  readonly protocol: string
  readonly package: string
  readonly model: (modelID: string, settings: Record<string, unknown>) => unknown
  readonly effortKey: "reasoningEffort" | "effort"
}) =>
  (["default", "low", "high", "max"] as const).map((effort) => {
    const settings = effort === "default" ? fixtureSettings : { apiKey: "fixture", [api.effortKey]: effort }
    return chatVariant({
      cassette: `moonshot-${api.name}/k3-streams-text-with-${effort}-effort`,
      protocol: api.protocol,
      provider: { package: api.package, modelID: "kimi-k3", settings },
      run: () =>
        LLMClient.generate(
          ai.request({
            id: `recorded_moonshot_${api.name}_k3_streams_text_with_${effort}_effort`,
            model: api.model("kimi-k3", settings),
            prompt: integerPrompt,
            maxTokens: 4096,
          }),
        ),
    })
  })

const moonshotLoopScenario = (api: {
  readonly name: "chat" | "messages" | "responses"
  readonly protocol: string
  readonly package: string
  readonly model: (modelID: string, settings: Record<string, unknown>) => unknown
  readonly modelID: string
  readonly settings: Record<string, unknown>
}) => {
  const slug = api.modelID.replaceAll(".", "-")
  return chatVariant({
    cassette: `moonshot-${api.name}/${slug}-preserves-reasoning-through-a-tool-loop-and-follow-up`,
    protocol: api.protocol,
    provider: { package: api.package, modelID: api.modelID, settings: api.settings },
    run: () =>
      runThreeStepWeatherLoop(
        ai.request({
          id: `recorded_moonshot_${api.name}_${slug}_tool_loop`,
          model: api.model(api.modelID, api.settings),
          prompt: weatherPrompt,
          tools: [chatWeather],
          maxTokens: 4096,
        }),
      ),
  })
}

const moonshotCapabilityScenarios = (api: {
  readonly name: "chat" | "messages" | "responses"
  readonly protocol: string
  readonly package: string
  readonly model: (modelID: string, settings: Record<string, unknown>) => unknown
  readonly rejectsToolChoice: boolean
  readonly structuredBody: Record<string, unknown>
}) => {
  const settings =
    api.name === "messages" ? { apiKey: "fixture", effort: "low" } : { apiKey: "fixture", reasoningEffort: "low" }
  const model = api.model("kimi-k3", settings)
  return [
    chatVariant({
      cassette: `moonshot-${api.name}/k3-reads-image-bytes`,
      protocol: api.protocol,
      provider: { package: api.package, modelID: "kimi-k3", settings },
      run: () =>
        Effect.gen(function* () {
          const image = yield* restroomImage()
          yield* LLMClient.generate(
            ai.request({
              id: `recorded_moonshot_${api.name}_k3_reads_image_bytes`,
              model,
              messages: [
                Message.user([
                  { type: "text", text: "Read the three words in this image. Reply only with those words in order." },
                  { type: "media", media: Media.bytes(image, "image/png") },
                ]),
              ],
              maxTokens: 4096,
            }),
          )
        }),
    }),
    ...(["none", "required"] as const).map((toolChoice) =>
      chatVariant({
        cassette: `moonshot-${api.name}/k3-${api.rejectsToolChoice ? "rejects-unsupported" : "respects"}-tool-choice-${toolChoice}`,
        protocol: api.protocol,
        provider: { package: api.package, modelID: "kimi-k3", settings },
        run: () => {
          const generate = LLMClient.generate(
            ai.request({
              id: `recorded_moonshot_${api.name}_k3_tool_choice_${toolChoice}`,
              model,
              prompt: "Use get_weather to look up the current weather in Paris.",
              tools: [chatWeather],
              toolChoice,
              maxTokens: 4096,
            }),
          )
          return api.rejectsToolChoice ? generate.pipe(Effect.flip) : generate
        },
      }),
    ),
    chatVariant({
      cassette: `moonshot-${api.name}/k3-returns-native-structured-output`,
      protocol: api.protocol,
      provider: { package: api.package, modelID: "kimi-k3", settings },
      run: () =>
        LLMClient.generate(
          ai.request({
            id: `recorded_moonshot_${api.name}_k3_returns_native_structured_output`,
            model,
            prompt: "Return a JSON object containing the capital city of France.",
            maxTokens: 4096,
            http: { body: api.structuredBody },
          }),
        ),
    }),
  ]
}

const zaiEffortScenarios = (["default", "low", "high", "max"] as const).map((effort) => {
  const settings = effort === "default" ? fixtureSettings : { apiKey: "fixture", reasoningEffort: effort }
  return chatVariant({
    cassette: `zai-chat/glm-5-3-streams-${effort}-effort`,
    protocol: "openai-chat",
    provider: { package: "@opencode/ai/providers/zai", modelID: "glm-5.3", settings },
    run: () =>
      LLMClient.generate(
        ai.request({
          id: `recorded_zai_glm_5_3_streams_${effort}_effort`,
          model: zaiModel("glm-5.3", settings),
          prompt: integerPrompt,
          maxTokens: 4096,
        }),
      ),
  })
})

const zaiThinkingScenarios = (["glm-4.7", "glm-5.2"] as const).flatMap((modelID) =>
  (["enabled", "disabled"] as const).map((type) => {
    const settings = { apiKey: "fixture", thinking: { type, clear_thinking: true } }
    const slug = modelID.replaceAll(".", "-")
    return chatVariant({
      cassette: `zai-chat/${slug}-streams-thinking-${type}`,
      protocol: "openai-chat",
      provider: { package: "@opencode/ai/providers/zai", modelID, settings },
      run: () =>
        LLMClient.generate(
          ai.request({
            id: `recorded_zai_${slug}_streams_thinking_${type}`,
            model: zaiModel(modelID, settings),
            prompt: integerPrompt,
            maxTokens: 4096,
          }),
        ),
    })
  }),
)

const zaiReasoningScenarios = (["none", "minimal", "low", "medium", "high", "xhigh", "max"] as const).map((effort) => {
  const settings = { apiKey: "fixture", reasoningEffort: effort }
  return chatVariant({
    cassette: `zai-chat/glm-5-2-streams-${effort}-effort`,
    protocol: "openai-chat",
    provider: { package: "@opencode/ai/providers/zai", modelID: "glm-5.2", settings },
    run: () =>
      LLMClient.generate(
        ai.request({
          id: `recorded_zai_glm_5_2_streams_${effort}_effort`,
          model: zaiModel("glm-5.2", settings),
          prompt: integerPrompt,
          maxTokens: 4096,
        }),
      ),
  })
})

const zaiToolLoopSettings = {
  apiKey: "fixture",
  reasoningEffort: "high",
  thinking: { type: "enabled", clear_thinking: false },
}
const zaiRejectSettings = { apiKey: "fixture", thinking: { type: "disabled" } }
const zaiImageSettings = { apiKey: "fixture", reasoningEffort: "low" }
const zaiJsonSettings = { apiKey: "fixture", reasoningEffort: "low", responseFormat: { type: "json_object" } }

const zaiScenarios = [
  ...zaiEffortScenarios,
  ...zaiThinkingScenarios,
  ...zaiReasoningScenarios,
  chatVariant({
    cassette: "zai-chat/glm-5-3-preserves-reasoning-through-a-tool-loop-and-follow-up",
    protocol: "openai-chat",
    provider: { package: "@opencode/ai/providers/zai", modelID: "glm-5.3", settings: zaiToolLoopSettings },
    run: () =>
      runThreeStepWeatherLoop(
        ai.request({
          id: "recorded_zai_glm_5_3_tool_loop",
          model: zaiModel("glm-5.3", zaiToolLoopSettings),
          prompt:
            "We have a budget of 38000 dollars for 219 trips costing 173 dollars each. Calculate whether that is affordable. If it is, use get_weather to look up the current weather in Paris. After receiving the result, report the weather in one short sentence.",
          tools: [chatWeather],
          maxTokens: 4096,
        }),
      ),
  }),
  chatVariant({
    cassette: "zai-chat/glm-5-3-rejects-disabled-thinking",
    protocol: "openai-chat",
    provider: { package: "@opencode/ai/providers/zai", modelID: "glm-5.3", settings: zaiRejectSettings },
    run: () =>
      LLMClient.generate(
        ai.request({
          id: "recorded_zai_glm_5_3_rejects_disabled_thinking",
          model: zaiModel("glm-5.3", zaiRejectSettings),
          prompt: integerPrompt,
          maxTokens: 4096,
        }),
      ).pipe(Effect.flip),
  }),
  chatVariant({
    cassette: "zai-chat/glm-5-3-flash-reads-image-bytes",
    protocol: "openai-chat",
    provider: { package: "@opencode/ai/providers/zai", modelID: "glm-5.3-flash", settings: zaiImageSettings },
    run: () =>
      Effect.gen(function* () {
        const bytes = yield* restroomImage()
        yield* LLMClient.generate(
          ai.request({
            id: "recorded_zai_glm_5_3_flash_reads_image_bytes",
            model: zaiModel("glm-5.3-flash", zaiImageSettings),
            messages: [
              Message.user([
                { type: "text", text: "Read the three words in this image. Reply with only the words in order." },
                { type: "media", media: Media.bytes(bytes, "image/png") },
              ]),
            ],
            maxTokens: 4096,
          }),
        )
      }),
  }),
  chatVariant({
    cassette: "zai-chat/glm-5-3-returns-a-json-object",
    protocol: "openai-chat",
    provider: { package: "@opencode/ai/providers/zai", modelID: "glm-5.3", settings: zaiJsonSettings },
    run: () =>
      LLMClient.generate(
        ai.request({
          id: "recorded_zai_glm_5_3_returns_a_json_object",
          model: zaiModel("glm-5.3", zaiJsonSettings),
          prompt: 'Return a JSON object with one key "city" set to the capital city of France.',
          maxTokens: 4096,
        }),
      ),
  }),
  chatVariant({
    cassette: "zai-chat/glm-4-5-calls-a-tool-without-streaming-arguments",
    protocol: "openai-chat",
    provider: { package: "@opencode/ai/providers/zai", modelID: "glm-4.5", settings: fixtureSettings },
    run: () =>
      LLMClient.generate(
        ai.request({
          id: "recorded_zai_glm_4_5_calls_a_tool_without_streaming_arguments",
          model: zaiModel("glm-4.5", fixtureSettings),
          prompt: "Use get_weather to look up the current weather in Paris.",
          tools: [legacyChatWeather],
          maxTokens: 4096,
        }),
      ),
  }),
]

const moonshotChatThinkingScenarios = [
  { modelID: "kimi-k2.6", name: "default", settings: fixtureSettings },
  {
    modelID: "kimi-k2.6",
    name: "disabled",
    settings: { apiKey: "fixture", thinking: { type: "disabled" } },
  },
  {
    modelID: "kimi-k2.6",
    name: "enabled",
    settings: { apiKey: "fixture", thinking: { type: "enabled", keep: null } },
  },
  { modelID: "kimi-k2.7-code", name: "default", settings: fixtureSettings },
  { modelID: "kimi-k2.7-code-highspeed", name: "default", settings: fixtureSettings },
].map((item) => {
  const slug = item.modelID.replaceAll(".", "-")
  return chatVariant({
    cassette: `moonshot-chat/${slug}-streams-text-with-${item.name}-thinking`,
    protocol: moonshotApi.chat.protocol,
    provider: { package: moonshotApi.chat.package, modelID: item.modelID, settings: item.settings },
    run: () =>
      LLMClient.generate(
        ai.request({
          id: `recorded_moonshot_chat_${slug}_streams_text_with_${item.name}_thinking`,
          model: moonshotApi.chat.model(item.modelID, item.settings),
          prompt: integerPrompt,
          maxTokens: 4096,
        }),
      ),
  })
})

const moonshotChatLoopScenarios = [
  { modelID: "kimi-k3", settings: { apiKey: "fixture", reasoningEffort: "low" } },
  { modelID: "kimi-k2.6", settings: { apiKey: "fixture", thinking: { type: "enabled", keep: "all" } } },
  { modelID: "kimi-k2.7-code", settings: fixtureSettings },
  { modelID: "kimi-k2.7-code-highspeed", settings: fixtureSettings },
].map((item) =>
  moonshotLoopScenario({
    name: "chat",
    protocol: moonshotApi.chat.protocol,
    package: moonshotApi.chat.package,
    model: moonshotApi.chat.model,
    ...item,
  }),
)

const minimaxAdaptiveSettings = { apiKey: "fixture", thinking: { type: "adaptive" } }
const minimaxDisabledSettings = { apiKey: "fixture", thinking: { type: "disabled" } }
const minimaxNoneSettings = { apiKey: "fixture", reasoningEffort: "none" }
const minimaxMinimalSettings = { apiKey: "fixture", reasoningEffort: "minimal" }

const minimaxScenarios = [
  chatVariant({
    cassette: "minimax-messages/m3-streams-text-with-thinking-disabled",
    protocol: "anthropic-messages",
    provider: {
      package: "@opencode/ai/providers/minimax",
      modelID: "MiniMax-M3",
      settings: minimaxDisabledSettings,
    },
    run: () =>
      LLMClient.generate(
        ai.request({
          id: "recorded_minimax_messages_m3_streams_text_with_thinking_disabled",
          model: minimaxMessagesModel("MiniMax-M3", minimaxDisabledSettings),
          prompt: integerPrompt,
          maxTokens: 1536,
        }),
      ),
  }),
  chatVariant({
    cassette: "minimax-messages/m3-streams-adaptive-thinking",
    protocol: "anthropic-messages",
    provider: {
      package: "@opencode/ai/providers/minimax",
      modelID: "MiniMax-M3",
      settings: minimaxAdaptiveSettings,
    },
    run: () =>
      LLMClient.generate(
        ai.request({
          id: "recorded_minimax_messages_m3_streams_adaptive_thinking",
          model: minimaxMessagesModel("MiniMax-M3", minimaxAdaptiveSettings),
          prompt: integerPrompt,
          maxTokens: 1536,
        }),
      ),
  }),
  chatVariant({
    cassette: "minimax-messages/m2-7-streams-default-thinking",
    protocol: "anthropic-messages",
    provider: { package: "@opencode/ai/providers/minimax", modelID: "MiniMax-M2.7", settings: fixtureSettings },
    run: () =>
      LLMClient.generate(
        ai.request({
          id: "recorded_minimax_messages_m2_7_streams_default_thinking",
          model: minimaxMessagesModel("MiniMax-M2.7", fixtureSettings),
          prompt: integerPrompt,
          maxTokens: 1536,
        }),
      ),
  }),
  chatVariant({
    cassette: "minimax-messages/m3-generates-a-named-tool-call-with-default-thinking-off",
    protocol: "anthropic-messages",
    provider: { package: "@opencode/ai/providers/minimax", modelID: "MiniMax-M3", settings: fixtureSettings },
    run: () =>
      LLMClient.generate(
        ai.request({
          id: "recorded_minimax_messages_m3_generates_a_named_tool_call_with_default_thinking_off",
          model: minimaxMessagesModel("MiniMax-M3", fixtureSettings),
          prompt: "Use get_weather to look up the current weather in Paris.",
          tools: [chatWeather],
          toolChoice: ToolChoice.named("get_weather"),
          maxTokens: 512,
        }),
      ),
  }),
  chatVariant({
    cassette: "minimax-messages/m3-continues-a-tool-loop-with-adaptive-thinking",
    protocol: "anthropic-messages",
    provider: {
      package: "@opencode/ai/providers/minimax",
      modelID: "MiniMax-M3",
      settings: minimaxAdaptiveSettings,
    },
    run: () =>
      runChatToolLoop(
        ai.request({
          id: "recorded_minimax_messages_m3_continues_a_tool_loop_with_adaptive_thinking",
          model: minimaxMessagesModel("MiniMax-M3", minimaxAdaptiveSettings),
          prompt: minimaxWeatherPrompt,
          tools: [chatWeather],
          toolChoice: "auto",
          maxTokens: 1536,
        }),
      ),
  }),
  chatVariant({
    cassette: "minimax-chat/m3-streams-text-with-thinking-disabled",
    protocol: "openai-chat",
    provider: {
      package: "@opencode/ai/providers/minimax/chat",
      modelID: "MiniMax-M3",
      settings: minimaxDisabledSettings,
    },
    run: () =>
      LLMClient.generate(
        ai.request({
          id: "recorded_minimax_chat_m3_streams_text_with_thinking_disabled",
          model: minimaxChatModel("MiniMax-M3", minimaxDisabledSettings),
          prompt: integerPrompt,
          maxTokens: 1536,
        }),
      ),
  }),
  chatVariant({
    cassette: "minimax-chat/m3-continues-a-tool-loop-with-default-thinking",
    protocol: "openai-chat",
    provider: { package: "@opencode/ai/providers/minimax/chat", modelID: "MiniMax-M3", settings: fixtureSettings },
    run: () =>
      runChatToolLoop(
        ai.request({
          id: "recorded_minimax_chat_m3_continues_a_tool_loop_with_default_thinking",
          model: minimaxChatModel("MiniMax-M3", fixtureSettings),
          prompt: minimaxWeatherPrompt,
          tools: [chatWeather],
          toolChoice: "auto",
          maxTokens: 1536,
        }),
      ),
  }),
  chatVariant({
    cassette: "minimax-responses/m3-streams-text-with-effort-none",
    protocol: "open-responses",
    provider: {
      package: "@opencode/ai/providers/minimax/responses",
      modelID: "MiniMax-M3",
      settings: minimaxNoneSettings,
    },
    run: () =>
      LLMClient.generate(
        ai.request({
          id: "recorded_minimax_responses_m3_streams_text_with_effort_none",
          model: minimaxResponsesModel("MiniMax-M3", minimaxNoneSettings),
          prompt: integerPrompt,
          maxTokens: 1536,
        }),
      ),
  }),
  chatVariant({
    cassette: "minimax-responses/m3-continues-a-tool-loop-with-effort-minimal",
    protocol: "open-responses",
    provider: {
      package: "@opencode/ai/providers/minimax/responses",
      modelID: "MiniMax-M3",
      settings: minimaxMinimalSettings,
    },
    run: () =>
      runChatToolLoop(
        ai.request({
          id: "recorded_minimax_responses_m3_continues_a_tool_loop_with_effort_minimal",
          model: minimaxResponsesModel("MiniMax-M3", minimaxMinimalSettings),
          prompt: minimaxWeatherPrompt,
          tools: [chatWeather],
          toolChoice: "auto",
          maxTokens: 1536,
        }),
      ),
  }),
]

const groqLookupWeather = ToolDefinition.make({
  name: "lookup_weather",
  description: "Look up the current weather for a city",
  inputSchema: {
    type: "object",
    properties: { city: { type: "string", enum: ["Paris", "London"] } },
    required: ["city"],
    additionalProperties: false,
  },
})
const mistralLookupWeather = ToolDefinition.make({
  name: "lookup_weather",
  description: "Look up the current weather for a city",
  inputSchema: {
    type: "object",
    properties: { city: { type: "string", enum: ["Paris"] } },
    required: ["city"],
    additionalProperties: false,
  },
})
const veniceLookupRate = ToolDefinition.make({
  name: "lookup_rate",
  description: "Retrieve the private conversion rate for a code. Only this tool knows the rate.",
  inputSchema: {
    type: "object",
    properties: { code: { type: "string", enum: ["ZEBRA", "MOOSE"] } },
    required: ["code"],
    additionalProperties: false,
  },
})

const groqTextSettings = {
  apiKey: "fixture",
  includeReasoning: false,
  reasoningEffort: "low",
  serviceTier: "on_demand",
  user: "recorded-test",
}

const groqScenarios = [
  chatVariant({
    cassette: "groq-chat/streams-text-with-usage",
    protocol: "openai-chat",
    provider: { package: "@opencode/ai/providers/groq", modelID: "openai/gpt-oss-20b", settings: groqTextSettings },
    run: () =>
      LLMClient.generate(
        ai.request({
          id: "recorded_groq_chat_streams_text_with_usage",
          model: Groq.model("openai/gpt-oss-20b", groqTextSettings),
          prompt: "Reply with exactly one word: hello",
          maxTokens: 512,
        }),
      ),
  }),
  ...[
    {
      cassette: "groq-chat/continues-qwen-parallel-tool-calls",
      modelID: "qwen/qwen3.6-27b",
      settings: { apiKey: "fixture", parallelToolCalls: true, reasoningEffort: "none" },
      prompt:
        "Look up the current weather in Paris and London. Call lookup_weather once for each city in the same response before answering. After receiving all results, report each city's weather in one short sentence.",
    },
    {
      cassette: "groq-chat/replays-gpt-oss-reasoning-through-a-tool-loop",
      modelID: "openai/gpt-oss-20b",
      settings: { apiKey: "fixture", includeReasoning: true, reasoningEffort: "low" },
      prompt:
        "Look up the current weather in Paris. Call lookup_weather once for each city in the same response before answering. After receiving all results, report each city's weather in one short sentence.",
    },
  ].map((item) =>
    chatVariant({
      cassette: item.cassette,
      protocol: "openai-chat",
      provider: { package: "@opencode/ai/providers/groq", modelID: item.modelID, settings: item.settings },
      run: () =>
        runChatToolLoop(
          ai.request({
            id: `recorded_${item.cassette.replaceAll(/[/-]/g, "_")}`,
            model: Groq.model(item.modelID, item.settings),
            prompt: item.prompt,
            tools: [groqLookupWeather],
            toolChoice: "required",
            maxTokens: 1536,
          }),
        ),
    }),
  ),
  chatVariant({
    cassette: "groq-chat/streams-qwen-parsed-reasoning",
    protocol: "openai-chat",
    provider: {
      package: "@opencode/ai/providers/groq",
      modelID: "qwen/qwen3.6-27b",
      settings: { apiKey: "fixture", reasoningEffort: "default" },
    },
    run: () =>
      LLMClient.generate(
        ai.request({
          id: "recorded_groq_chat_streams_qwen_parsed_reasoning",
          model: Groq.model("qwen/qwen3.6-27b", { apiKey: "fixture", reasoningEffort: "default" }),
          prompt:
            "What is 173 multiplied by 219? Think through the arithmetic, then reply with only the final integer.",
          maxTokens: 2048,
        }),
      ),
  }),
]

const veniceSystem =
  "Use the provided tool for private data. After receiving the result, give only the requested number; do not call the tool again."
const venicePrompt =
  "Look up conversion rate for code ZEBRA. Then multiply that rate by 17 and add 9. You must use lookup_rate before answering."
const veniceFollowUp = "Add 7 to your previous final answer. Reply with just the number."

const veniceLoopScenarios = [
  { modelID: "deepseek-v4-flash", settings: fixtureSettings },
  { modelID: "claude-opus-4-8", settings: { apiKey: "fixture", reasoningEffort: "high" } },
  { modelID: "gemini-3-1-pro-preview", settings: { apiKey: "fixture", reasoningEffort: "high" } },
  { modelID: "openai-gpt-54-mini", settings: fixtureSettings },
].map((item) =>
  chatVariant({
    cassette: `venice-chat/${item.modelID}-preserves-reasoning-through-a-tool-loop-and-follow-up`,
    protocol: "openai-chat",
    provider: { package: "@opencode/ai/providers/venice", modelID: item.modelID, settings: item.settings },
    run: () =>
      Effect.gen(function* () {
        const request = ai.request({
          id: `recorded_venice_chat_${item.modelID.replaceAll("-", "_")}_tool_loop`,
          model: Venice.model(item.modelID, item.settings),
          system: veniceSystem,
          prompt: venicePrompt,
          tools: [veniceLookupRate],
          maxTokens: 4096,
        })
        const first = yield* LLMClient.generate(request)
        const continuation = {
          ...request,
          toolChoice: ToolChoice.make("none"),
          messages: [
            ...request.messages,
            first.message,
            ...first.toolCalls.map((call: any) =>
              Message.tool({ id: call.id, name: call.name, result: { code: "ZEBRA", rate: 23 } }),
            ),
          ],
        }
        const second = yield* LLMClient.generate(continuation)
        yield* LLMClient.generate({
          ...continuation,
          messages: [...continuation.messages, second.message, Message.user(veniceFollowUp)],
        })
      }),
  }),
)

const veniceParallelSettings = { apiKey: "fixture", reasoningEffort: "high", parallelToolCalls: true }

const veniceScenarios = [
  ...veniceLoopScenarios,
  chatVariant({
    cassette: "venice-chat/gemini-preserves-per-call-signatures-through-parallel-tools",
    protocol: "openai-chat",
    provider: {
      package: "@opencode/ai/providers/venice",
      modelID: "gemini-3-8-flash",
      settings: veniceParallelSettings,
    },
    run: () =>
      Effect.gen(function* () {
        const request = ai.request({
          id: "recorded_venice_chat_gemini_parallel_tools",
          model: Venice.model("gemini-3-8-flash", veniceParallelSettings),
          prompt:
            "Look up private rates for both ZEBRA and MOOSE using two calls to lookup_rate. Then add the rates and multiply by 7. Use parallel calls if possible.",
          tools: [veniceLookupRate],
          maxTokens: 4096,
        })
        const first = yield* LLMClient.generate(request)
        const continuation = {
          ...request,
          toolChoice: ToolChoice.make("none"),
          messages: [
            ...request.messages,
            first.message,
            ...first.toolCalls.map((call: any) =>
              Message.tool({
                id: call.id,
                name: call.name,
                result: { rate: call.input.code === "ZEBRA" ? 23 : 31 },
              }),
            ),
          ],
        }
        const second = yield* LLMClient.generate(continuation)
        yield* LLMClient.generate({
          ...continuation,
          messages: [...continuation.messages, second.message, Message.user(veniceFollowUp)],
        })
      }),
  }),
  chatVariant({
    cassette: "venice-chat/qwen-disables-reasoning",
    protocol: "openai-chat",
    provider: {
      package: "@opencode/ai/providers/venice",
      modelID: "qwen3-6-27b",
      settings: { apiKey: "fixture", reasoning: { enabled: false } },
    },
    run: () =>
      LLMClient.generate(
        ai.request({
          id: "recorded_venice_chat_qwen_disables_reasoning",
          model: Venice.model("qwen3-6-27b", { apiKey: "fixture", reasoning: { enabled: false } }),
          prompt: "What is 23 multiplied by 17 plus 9? Reply with just the number.",
          maxTokens: 4096,
        }),
      ),
  }),
  chatVariant({
    cassette: "venice-chat/surfaces-venice-model-errors",
    protocol: "openai-chat",
    provider: { package: "@opencode/ai/providers/venice", modelID: "no-such-model-xyz", settings: fixtureSettings },
    run: () =>
      LLMClient.generate(
        ai.request({
          id: "recorded_venice_chat_surfaces_model_errors",
          model: Venice.model("no-such-model-xyz", fixtureSettings),
          prompt: "Hello",
        }),
      ).pipe(Effect.flip),
  }),
]

const digitalOceanChatScenarios = [
  { slug: "haiku", modelID: "anthropic-claude-haiku-4.5", maxTokens: 128, settings: fixtureSettings },
  {
    slug: "gpt-nano",
    modelID: "openai-gpt-5-nano",
    maxTokens: 1024,
    settings: { apiKey: "fixture", reasoningEffort: "minimal" },
  },
].flatMap((item) => {
  const model = () => DigitalOcean.model(item.modelID, item.settings)
  return [
    chatVariant({
      cassette: `digitalocean-chat/${item.slug}-reuses-a-cached-prompt`,
      protocol: "openai-chat",
      provider: { package: "@opencode/ai/providers/digitalocean", modelID: item.modelID, settings: item.settings },
      run: () =>
        Effect.gen(function* () {
          const request = ai.request({
            id: `recorded_digitalocean_chat_${item.slug}_reuses_a_cached_prompt`,
            model: model(),
            system: LARGE_CACHEABLE_SYSTEM,
            prompt: "Reply exactly: OK",
            promptCacheKey: `digitalocean-recorded-${item.modelID}`,
            maxTokens: item.maxTokens,
          })
          yield* LLMClient.generate(request)
          yield* LLMClient.generate(request)
        }),
    }),
    chatVariant({
      cassette: `digitalocean-chat/${item.slug}-continues-a-tool-call-with-cache-markers`,
      protocol: "openai-chat",
      provider: { package: "@opencode/ai/providers/digitalocean", modelID: item.modelID, settings: item.settings },
      run: () =>
        runWeatherToolLoop({
          ...goldenWeatherToolLoopRequest({
            id: `digitalocean-${item.modelID}-tool-loop`,
            model: model(),
            maxTokens: item.maxTokens,
            temperature: false,
          }),
          cache: "auto",
        }),
    }),
  ]
})

const digitalOceanResponsesScenarios = [
  chatVariant({
    cassette: "digitalocean-responses/streams-text",
    protocol: "open-responses",
    provider: {
      package: "@opencode/ai/providers/digitalocean/responses",
      modelID: "openai-gpt-5.4-mini",
      settings: fixtureSettings,
    },
    run: () =>
      LLMClient.generate(
        ai.request({
          id: "recorded_digitalocean_responses_streams_text",
          model: DigitalOcean.responsesModel("openai-gpt-5.4-mini", fixtureSettings),
          prompt: "Reply exactly: OK",
          maxTokens: 64,
        }),
      ),
  }),
  chatVariant({
    cassette: "digitalocean-responses/continues-a-tool-call",
    protocol: "open-responses",
    provider: {
      package: "@opencode/ai/providers/digitalocean/responses",
      modelID: "openai-gpt-5.4-mini",
      settings: fixtureSettings,
    },
    run: () =>
      runWeatherToolLoop(
        weatherToolLoopRequest({
          id: "digitalocean-responses-tool-loop",
          model: DigitalOcean.responsesModel("openai-gpt-5.4-mini", fixtureSettings),
          maxTokens: 256,
          temperature: false,
        }),
      ),
  }),
  chatVariant({
    cassette: "digitalocean-responses/calls-a-freeform-text-tool",
    protocol: "open-responses",
    provider: {
      package: "@opencode/ai/providers/digitalocean/responses",
      modelID: "openai-gpt-5.4-mini",
      settings: fixtureSettings,
    },
    run: () =>
      LLMClient.generate(
        ai.request({
          id: "recorded_digitalocean_responses_calls_a_freeform_text_tool",
          model: DigitalOcean.responsesModel("openai-gpt-5.4-mini", fixtureSettings),
          prompt: "Use the run tool to execute exactly this code: print(1)",
          tools: freeformRun,
          toolChoice: "required",
          maxTokens: 256,
        }),
      ),
  }),
]

const openRouterResponsesScenarios = [
  chatVariant({
    cassette: "openrouter-responses/streams-text",
    protocol: "open-responses",
    provider: {
      package: "@opencode/ai/providers/openrouter/responses",
      modelID: "openai/gpt-5.4-mini",
      settings: fixtureSettings,
    },
    run: () =>
      LLMClient.generate(
        ai.request({
          id: "recorded_openrouter_responses_streams_text",
          model: OpenRouter.responsesModel("openai/gpt-5.4-mini", fixtureSettings),
          prompt: "Reply exactly: OK",
          maxTokens: 64,
        }),
      ),
  }),
  chatVariant({
    cassette: "openrouter-responses/continues-a-tool-call",
    protocol: "open-responses",
    provider: {
      package: "@opencode/ai/providers/openrouter/responses",
      modelID: "openai/gpt-5.4-mini",
      settings: fixtureSettings,
    },
    run: () =>
      runWeatherToolLoop(
        weatherToolLoopRequest({
          id: "openrouter-responses-tool-loop",
          model: OpenRouter.responsesModel("openai/gpt-5.4-mini", fixtureSettings),
          maxTokens: 256,
          temperature: false,
        }),
      ),
  }),
  chatVariant({
    cassette: "openrouter-responses/calls-a-freeform-text-tool",
    protocol: "open-responses",
    provider: {
      package: "@opencode/ai/providers/openrouter/responses",
      modelID: "openai/gpt-5.4-mini",
      settings: fixtureSettings,
    },
    run: () =>
      LLMClient.generate(
        ai.request({
          id: "recorded_openrouter_responses_calls_a_freeform_text_tool",
          model: OpenRouter.responsesModel("openai/gpt-5.4-mini", fixtureSettings),
          prompt: "Use the run tool to execute exactly this code: print(1)",
          tools: freeformRun,
          toolChoice: "required",
          maxTokens: 256,
        }),
      ),
  }),
]

const openRouterReasoningSettings = { apiKey: "fixture", reasoning: { max_tokens: 1024 } }
const openRouterReasoningScenarios = [
  chatVariant({
    cassette: "openrouter-reasoning",
    protocol: "openai-chat",
    provider: {
      package: "@opencode/ai/providers/openrouter",
      modelID: "anthropic/claude-sonnet-4.6",
      settings: openRouterReasoningSettings,
    },
    run: () =>
      LLMClient.generate(
        ai.request({
          id: "recorded_openrouter_reasoning",
          model: OpenRouter.model("anthropic/claude-sonnet-4.6", openRouterReasoningSettings),
          system: "Think through the arithmetic, then reply with only the final integer.",
          prompt: "What is 173 multiplied by 219?",
          maxTokens: 1536,
          temperature: 0,
        }),
      ),
  }),
  chatVariant({
    cassette: "openrouter-reasoning-tool-loop",
    protocol: "openai-chat",
    provider: {
      package: "@opencode/ai/providers/openrouter",
      modelID: "anthropic/claude-sonnet-4.6",
      settings: openRouterReasoningSettings,
    },
    run: () =>
      runWeatherToolLoop(
        goldenWeatherToolLoopRequest({
          id: "openrouter-reasoning-tool-loop",
          model: OpenRouter.model("anthropic/claude-sonnet-4.6", openRouterReasoningSettings),
          maxTokens: 1536,
          temperature: false,
        }),
      ),
  }),
]

const mistralScenarios = [
  chatVariant({
    cassette: "mistral-chat/streams-text-with-usage",
    protocol: "openai-chat",
    provider: {
      package: "@opencode/ai/providers/mistral",
      modelID: "mistral-small-latest",
      settings: { apiKey: "fixture", reasoningEffort: "none" },
    },
    run: () =>
      LLMClient.generate(
        ai.request({
          id: "recorded_mistral_chat_streams_text_with_usage",
          model: Mistral.model("mistral-small-latest", { apiKey: "fixture", reasoningEffort: "none" }),
          prompt: "Reply with exactly one word: hello",
          maxTokens: 40,
          temperature: 0,
        }),
      ),
  }),
  chatVariant({
    cassette: "mistral-chat/replays-native-reasoning",
    protocol: "openai-chat",
    provider: {
      package: "@opencode/ai/providers/mistral",
      modelID: "mistral-small-latest",
      settings: { apiKey: "fixture", reasoningEffort: "high" },
    },
    run: () =>
      Effect.gen(function* () {
        const firstRequest = ai.request({
          id: "recorded_mistral_chat_replays_native_reasoning",
          model: Mistral.model("mistral-small-latest", { apiKey: "fixture", reasoningEffort: "high" }),
          prompt: "Calculate 17 multiplied by 23. Think briefly, then reply with only the integer.",
          maxTokens: 512,
          temperature: 0,
        })
        const first = yield* LLMClient.generate(firstRequest)
        yield* LLMClient.generate({
          ...firstRequest,
          messages: [...firstRequest.messages, first.message, Message.user("Reply with exactly: Done.")],
          maxTokens: 256,
          temperature: 0,
        })
      }),
  }),
  chatVariant({
    cassette: "mistral-chat/drives-a-tool-loop",
    protocol: "openai-chat",
    provider: {
      package: "@opencode/ai/providers/mistral",
      modelID: "mistral-small-latest",
      settings: { apiKey: "fixture", reasoningEffort: "none" },
    },
    run: () =>
      Effect.gen(function* () {
        const firstRequest = ai.request({
          id: "recorded_mistral_chat_drives_a_tool_loop",
          model: Mistral.model("mistral-small-latest", { apiKey: "fixture", reasoningEffort: "none" }),
          system: [
            SystemPart.make("Call lookup_weather exactly once with Paris."),
            SystemPart.make("After the tool result, describe the weather briefly."),
          ],
          prompt: "What is the weather?",
          tools: [mistralLookupWeather],
          toolChoice: mistralLookupWeather,
          maxTokens: 160,
          temperature: 0,
        })
        const first = yield* LLMClient.generate(firstRequest)
        yield* LLMClient.generate({
          ...firstRequest,
          toolChoice: ToolChoice.make("none"),
          messages: [
            ...firstRequest.messages,
            first.message,
            ...first.toolCalls.map((call: any) =>
              Message.tool({ id: call.id, name: call.name, result: { condition: "sunny", temperature: "18C" } }),
            ),
          ],
          maxTokens: 160,
          temperature: 0,
        })
      }),
  }),
  chatVariant({
    cassette: "mistral-chat-glm/streams-an-indexed-tool-call",
    protocol: "openai-chat",
    provider: { package: "@opencode/ai/providers/mistral", modelID: "zai-glm-5-2", settings: fixtureSettings },
    run: () =>
      LLMClient.generate(
        ai.request({
          id: "recorded_mistral_chat_glm_streams_an_indexed_tool_call",
          model: Mistral.model("zai-glm-5-2", fixtureSettings),
          system: "Call lookup_weather exactly once with Paris.",
          prompt: "What is the weather?",
          tools: [mistralLookupWeather],
          toolChoice: mistralLookupWeather,
          maxTokens: 256,
          temperature: 0,
        }),
      ),
  }),
]

const chatVariantScenarios = [
  ...groqScenarios,
  ...veniceScenarios,
  ...digitalOceanChatScenarios,
  ...digitalOceanResponsesScenarios,
  ...openRouterResponsesScenarios,
  ...openRouterReasoningScenarios,
  ...mistralScenarios,
  ...zaiScenarios,
  ...moonshotEffortScenarios({
    name: "chat",
    protocol: moonshotApi.chat.protocol,
    package: moonshotApi.chat.package,
    model: moonshotApi.chat.model,
    effortKey: "reasoningEffort",
  }),
  ...moonshotEffortScenarios({
    name: "messages",
    protocol: moonshotApi.messages.protocol,
    package: moonshotApi.messages.package,
    model: moonshotApi.messages.model,
    effortKey: "effort",
  }),
  ...moonshotEffortScenarios({
    name: "responses",
    protocol: moonshotApi.responses.protocol,
    package: moonshotApi.responses.package,
    model: moonshotApi.responses.model,
    effortKey: "reasoningEffort",
  }),
  ...moonshotChatThinkingScenarios,
  ...moonshotChatLoopScenarios,
  moonshotLoopScenario({
    name: "messages",
    protocol: moonshotApi.messages.protocol,
    package: moonshotApi.messages.package,
    model: moonshotApi.messages.model,
    modelID: "kimi-k3",
    settings: { apiKey: "fixture", effort: "low" },
  }),
  moonshotLoopScenario({
    name: "responses",
    protocol: moonshotApi.responses.protocol,
    package: moonshotApi.responses.package,
    model: moonshotApi.responses.model,
    modelID: "kimi-k3",
    settings: { apiKey: "fixture", reasoningEffort: "low" },
  }),
  ...moonshotCapabilityScenarios({
    name: "chat",
    protocol: moonshotApi.chat.protocol,
    package: moonshotApi.chat.package,
    model: moonshotApi.chat.model,
    rejectsToolChoice: false,
    structuredBody: {
      response_format: { type: "json_schema", json_schema: { name: "location", schema: locationSchema, strict: true } },
    },
  }),
  ...moonshotCapabilityScenarios({
    name: "messages",
    protocol: moonshotApi.messages.protocol,
    package: moonshotApi.messages.package,
    model: moonshotApi.messages.model,
    rejectsToolChoice: false,
    structuredBody: { output_config: { format: { type: "json_schema", schema: locationSchema } } },
  }),
  ...moonshotCapabilityScenarios({
    name: "responses",
    protocol: moonshotApi.responses.protocol,
    package: moonshotApi.responses.package,
    model: moonshotApi.responses.model,
    rejectsToolChoice: true,
    structuredBody: {
      text: { format: { type: "json_schema", name: "location", schema: locationSchema, strict: true } },
    },
  }),
  ...minimaxScenarios,
]

const vendorVariant = (scenario: Scenario) => ({ group: "vendor-variants", scenario })
const nativeVendor = (scenario: Scenario) => ({ group: "native-vendors", scenario })

const alibabaSettings = { region: "ap-southeast-1", apiKey: "fixture" }
const metaSettings = { apiKey: "fixture" }
const xaiSettings = { apiKey: "fixture" }

const alibabaLoopPrompt =
  "We have a budget of 38000 dollars for 219 trips costing 173 dollars each. Calculate whether that is affordable. If it is, use get_weather to look up the current weather in Paris. After receiving the result, report the weather in one short sentence."
const metaLoopPrompt =
  "Look up the current weather in Paris using lookup_weather before answering. After receiving the result, report Paris's weather in one short sentence."
const metaMessagesLoopPrompt =
  "Look up the current weather in Paris using lookup_weather. After receiving the result, report Paris's weather in one short sentence."
const metaWeather = ToolDefinition.make({
  name: "lookup_weather",
  description: "Look up the current weather for a city",
  inputSchema: {
    type: "object",
    properties: { city: { type: "string", enum: ["Paris"] } },
    required: ["city"],
    additionalProperties: false,
  },
})
const metaMessagesWeather = ToolDefinition.make({
  name: "lookup_weather",
  description: "Look up current weather",
  inputSchema: {
    type: "object",
    properties: { city: { type: "string", enum: ["Paris"] } },
    required: ["city"],
    additionalProperties: false,
  },
})

const alibabaApi = {
  chat: {
    name: "chat",
    protocol: "alibaba-chat",
    package: "@opencode/ai/providers/alibaba",
    model: (modelID: string) => Alibaba.configure(alibabaSettings).chat(modelID),
  },
  messages: {
    name: "messages",
    protocol: "alibaba-messages",
    package: "@opencode/ai/providers/alibaba/messages",
    model: (modelID: string) => Alibaba.configure(alibabaSettings).messages(modelID),
  },
  responses: {
    name: "responses",
    protocol: "alibaba-responses",
    package: "@opencode/ai/providers/alibaba/responses",
    model: (modelID: string) => Alibaba.configure(alibabaSettings).responses(modelID),
  },
} as const

const metaApi = {
  chat: {
    name: "chat",
    protocol: "openai-chat",
    package: "@opencode/ai/providers/meta/chat",
    model: (modelID: string) => Meta.configure(metaSettings).chat(modelID),
  },
  responses: {
    name: "responses",
    protocol: "meta-responses",
    package: "@opencode/ai/providers/meta/responses",
    model: (modelID: string) => Meta.configure(metaSettings).responses(modelID),
  },
  messages: {
    name: "messages",
    protocol: "meta-messages",
    package: "@opencode/ai/providers/meta/messages",
    model: (modelID: string) => Meta.configure(metaSettings).messages(modelID),
  },
} as const

type AlibabaApi = (typeof alibabaApi)[keyof typeof alibabaApi]

const alibabaEffortScenarios = (
  api: AlibabaApi,
  efforts: ReadonlyArray<string | undefined>,
  effortKey: "reasoningEffort" | "effort",
) =>
  efforts.map((effort) =>
    vendorVariant({
      cassette: `alibaba-${api.name}/qwen-3-8-max-streams-${effort ?? "default"}-effort`,
      protocol: api.protocol,
      provider: { package: api.package, modelID: "qwen3.8-max", settings: alibabaSettings },
      run: () =>
        LLMClient.generate(
          ai.request({
            id: `recorded_alibaba_${api.name}_qwen_3_8_max_streams_${effort ?? "default"}_effort`,
            model: api.model("qwen3.8-max"),
            prompt: integerPrompt,
            ...(effort === undefined ? {} : { providerOptions: { [effortKey]: effort } }),
            maxTokens: 4096,
          }),
        ),
    }),
  )

const alibabaThinkingScenarios = (api: AlibabaApi) =>
  ([false, true] as const).map((enabled) =>
    vendorVariant({
      cassette: `alibaba-${api.name}/qwen-3-7-plus-streams-thinking-${enabled ? "enabled" : "disabled"}`,
      protocol: api.protocol,
      provider: { package: api.package, modelID: "qwen3.7-plus", settings: alibabaSettings },
      run: () =>
        LLMClient.generate(
          ai.request({
            id: `recorded_alibaba_${api.name}_qwen_3_7_plus_streams_thinking_${enabled ? "enabled" : "disabled"}`,
            model: api.model("qwen3.7-plus"),
            providerOptions:
              api.name === "messages"
                ? { thinking: { type: enabled ? "enabled" : "disabled", ...(enabled ? { budgetTokens: 1024 } : {}) } }
                : api.name === "chat"
                  ? { enableThinking: enabled, ...(enabled ? { thinkingBudget: 1024 } : {}) }
                  : { enableThinking: enabled },
            prompt: integerPrompt,
            maxTokens: 4096,
          }),
        ),
    }),
  )

const alibabaCapabilityScenarios = (api: AlibabaApi) => [
  vendorVariant({
    cassette: `alibaba-${api.name}/qwen-3-8-flash-reads-image-bytes`,
    protocol: api.protocol,
    provider: { package: api.package, modelID: "qwen3.8-flash", settings: alibabaSettings },
    run: () =>
      Effect.gen(function* () {
        const image = yield* restroomImage()
        yield* LLMClient.generate(
          ai.request({
            id: `recorded_alibaba_${api.name}_qwen_3_8_flash_reads_image_bytes`,
            model: api.model("qwen3.8-flash"),
            providerOptions: api.name === "messages" ? { thinking: { type: "disabled" } } : { enableThinking: false },
            messages: [
              Message.user([
                { type: "text", text: "Read the three words in this image. Reply with only the words in order." },
                { type: "media", media: Media.bytes(image, "image/png") },
              ]),
            ],
            maxTokens: 4096,
          }),
        )
      }),
  }),
  vendorVariant({
    cassette: `alibaba-${api.name}/qwen-3-8-max-obeys-named-tool-choice`,
    protocol: api.protocol,
    provider: { package: api.package, modelID: "qwen3.8-max", settings: alibabaSettings },
    run: () =>
      LLMClient.generate(
        ai.request({
          id: `recorded_alibaba_${api.name}_qwen_3_8_max_obeys_named_tool_choice`,
          model: api.model("qwen3.8-max"),
          prompt: "Find the current weather in Paris.",
          providerOptions: api.name === "messages" ? { thinking: { type: "disabled" } } : { reasoningEffort: "none" },
          tools: [legacyChatWeather],
          toolChoice: { type: "tool", name: "get_weather" },
          maxTokens: 4096,
        }),
      ),
  }),
]

const alibabaLoopScenarios = (api: AlibabaApi) => [
  vendorVariant({
    cassette: `alibaba-${api.name}/qwen-3-8-max-replays-reasoning-through-a-tool-loop-and-follow-up`,
    protocol: api.protocol,
    provider: { package: api.package, modelID: "qwen3.8-max", settings: alibabaSettings },
    run: () =>
      runThreeStepWeatherLoop(
        ai.request({
          id: `recorded_alibaba_${api.name}_qwen_3_8_max_tool_loop`,
          model: api.model("qwen3.8-max"),
          providerOptions:
            api.name === "messages"
              ? { effort: "medium" }
              : api.name === "chat"
                ? { reasoningEffort: "medium", preserveThinking: true, toolStream: true }
                : { reasoningEffort: "medium", store: false },
          prompt: alibabaLoopPrompt,
          tools: [chatWeather],
          maxTokens: 4096,
        }),
      ),
  }),
]

const alibabaResponseExtraScenarios = [
  vendorVariant({
    cassette: "alibaba-chat/qwen-3-8-max-returns-a-json-object",
    protocol: "alibaba-chat",
    provider: { package: "@opencode/ai/providers/alibaba", modelID: "qwen3.8-max", settings: alibabaSettings },
    run: () =>
      LLMClient.generate(
        ai.request({
          id: "recorded_alibaba_chat_qwen_3_8_max_returns_a_json_object",
          model: alibabaApi.chat.model("qwen3.8-max"),
          prompt: 'Return a JSON object with one key "city" set to the capital city of France.',
          providerOptions: { reasoningEffort: "none", responseFormat: { type: "json_object" } },
          maxTokens: 1024,
        }),
      ),
  }),
  vendorVariant({
    cassette: "alibaba-messages/qwen-3-8-max-follows-a-json-schema",
    protocol: "alibaba-messages",
    provider: {
      package: "@opencode/ai/providers/alibaba/messages",
      modelID: "qwen3.8-max",
      settings: alibabaSettings,
    },
    run: () =>
      LLMClient.generate(
        ai.request({
          id: "recorded_alibaba_messages_qwen_3_8_max_follows_a_json_schema",
          model: alibabaApi.messages.model("qwen3.8-max"),
          prompt: 'Return a JSON object with one key "city" set to the capital city of France.',
          providerOptions: {
            thinking: { type: "disabled" },
            outputConfig: {
              format: {
                type: "json_schema",
                schema: {
                  type: "object",
                  properties: { city: { type: "string" } },
                  required: ["city"],
                  additionalProperties: false,
                },
              },
            },
          },
          maxTokens: 1024,
        }),
      ),
  }),
  vendorVariant({
    cassette: "alibaba-responses/qwen-3-8-max-continues-a-stored-response",
    protocol: "alibaba-responses",
    provider: {
      package: "@opencode/ai/providers/alibaba/responses",
      modelID: "qwen3.8-max",
      settings: alibabaSettings,
    },
    run: () =>
      Effect.gen(function* () {
        const first = yield* LLMClient.generate(
          ai.request({
            id: "recorded_alibaba_responses_qwen_3_8_max_continues_a_stored_response",
            model: alibabaApi.responses.model("qwen3.8-max"),
            prompt: "Remember the password word apricot. Reply OK.",
            providerOptions: { store: true, reasoningEffort: "none" },
            maxTokens: 1024,
          }),
        )
        const id = first.events.find(LLMEvent.is.finish)?.providerMetadata?.alibaba?.responseId
        yield* LLMClient.generate(
          ai.request({
            id: "recorded_alibaba_responses_qwen_3_8_max_continues_a_stored_response_follow_up",
            model: alibabaApi.responses.model("qwen3.8-max"),
            prompt: "What word did I ask you to remember? Reply with only the word.",
            providerOptions: { previousResponseId: id, store: true, reasoningEffort: "none" },
            maxTokens: 1024,
          }),
        )
      }),
  }),
  vendorVariant({
    cassette: "alibaba-responses/qwen-3-8-max-uses-hosted-web-search-and-extraction",
    protocol: "alibaba-responses",
    provider: {
      package: "@opencode/ai/providers/alibaba/responses",
      modelID: "qwen3.8-max",
      settings: alibabaSettings,
    },
    run: () =>
      LLMClient.generate(
        ai.request({
          id: "recorded_alibaba_responses_qwen_3_8_max_uses_hosted_web_search_and_extraction",
          model: alibabaApi.responses.model("qwen3.8-max"),
          prompt:
            "Use web search to find Alibaba Cloud Model Studio's official documentation, then use web_extractor to read the page. Give a brief summary with the source URL.",
          tools: [Alibaba.webSearch(), Alibaba.webExtractor()],
          providerOptions: { reasoningEffort: "low", store: false },
          maxTokens: 4096,
        }),
      ),
  }),
  vendorVariant({
    cassette: "alibaba-responses/qwen-3-8-max-uses-hosted-code-interpreter",
    protocol: "alibaba-responses",
    provider: {
      package: "@opencode/ai/providers/alibaba/responses",
      modelID: "qwen3.8-max",
      settings: alibabaSettings,
    },
    run: () =>
      LLMClient.generate(
        ai.request({
          id: "recorded_alibaba_responses_qwen_3_8_max_uses_hosted_code_interpreter",
          model: alibabaApi.responses.model("qwen3.8-max"),
          prompt:
            "Use the code interpreter to compute the SHA-256 hash of the UTF-8 string hello (no newline). Reply with only the hash.",
          tools: [Alibaba.codeInterpreter()],
          providerOptions: { reasoningEffort: "low" },
          maxTokens: 4096,
        }),
      ),
  }),
]

const metaEffortScenarios = (api: (typeof metaApi)["chat"] | (typeof metaApi)["responses"]) =>
  [undefined, "minimal", "low", "medium", "high", "xhigh", "max"].map((effort) =>
    vendorVariant({
      cassette: `meta-${api.name}/streams-text-with-${effort ?? "default"}-reasoning`,
      protocol: api.protocol,
      provider: { package: api.package, modelID: "muse-spark-1.3", settings: metaSettings },
      run: () =>
        LLMClient.generate(
          ai.request({
            id: `recorded_meta_${api.name}_streams_text_with_${effort ?? "default"}_reasoning`,
            model: api.model("muse-spark-1.3"),
            prompt: integerPrompt,
            ...(effort === undefined
              ? {}
              : {
                  providerOptions:
                    api.name === "responses" && effort === "high"
                      ? { reasoningEffort: effort, reasoningSummary: "auto" }
                      : { reasoningEffort: effort },
                }),
            maxTokens: 1024,
          }),
        ),
    }),
  )

const metaLoopScenarios = (api: (typeof metaApi)["chat"] | (typeof metaApi)["responses"]) => [
  vendorVariant({
    cassette: `meta-${api.name}/${api.name === "responses" ? "replays-encrypted-reasoning-through-a-tool-loop" : "continues-a-generated-tool-call"}`,
    protocol: api.protocol,
    provider: { package: api.package, modelID: "muse-spark-1.3", settings: metaSettings },
    run: () =>
      Effect.gen(function* () {
        const request = ai.request({
          id: `recorded_meta_${api.name}_${api.name === "responses" ? "replays_encrypted_reasoning_through_a_tool_loop" : "continues_a_generated_tool_call"}`,
          model: api.model("muse-spark-1.3"),
          prompt: metaLoopPrompt,
          tools: [metaWeather],
          toolChoice: "auto",
          providerOptions: { reasoningEffort: "low" },
          maxTokens: 1024,
        })
        const first = yield* LLMClient.generate(request)
        yield* LLMClient.generate({
          ...request,
          messages: [
            ...request.messages,
            first.message,
            ...first.toolCalls.map((call: any) =>
              Message.tool({ id: call.id, name: call.name, result: { condition: "sunny", temperature: "18C" } }),
            ),
          ],
        })
      }),
  }),
]

const metaMessagesScenarios = [
  ...(["adaptive", "enabled"] as const).map((mode) =>
    vendorVariant({
      cassette: `meta-messages/streams-text-with-${mode}-thinking`,
      protocol: "meta-messages",
      provider: { package: "@opencode/ai/providers/meta/messages", modelID: "muse-spark-1.3", settings: metaSettings },
      run: () =>
        LLMClient.generate(
          ai.request({
            id: `recorded_meta_messages_streams_text_with_${mode}_thinking`,
            model: metaApi.messages.model("muse-spark-1.3"),
            prompt: integerPrompt,
            maxTokens: 2048,
            providerOptions: {
              thinking:
                mode === "adaptive"
                  ? { type: "adaptive", display: "omitted" }
                  : { type: "enabled", budgetTokens: 1024, display: "omitted" },
              effort: "low",
            },
          }),
        ),
    }),
  ),
  vendorVariant({
    cassette: "meta-messages/replays-encrypted-thinking-through-a-tool-loop",
    protocol: "meta-messages",
    provider: { package: "@opencode/ai/providers/meta/messages", modelID: "muse-spark-1.3", settings: metaSettings },
    run: () =>
      Effect.gen(function* () {
        const request = ai.request({
          id: "recorded_meta_messages_replays_encrypted_thinking_through_a_tool_loop",
          model: metaApi.messages.model("muse-spark-1.3"),
          prompt: metaMessagesLoopPrompt,
          tools: [metaMessagesWeather],
          toolChoice: "auto",
          maxTokens: 1024,
          providerOptions: { effort: "low" },
        })
        const first = yield* LLMClient.generate(request)
        yield* LLMClient.generate({
          ...request,
          toolChoice: ToolChoice.make("none"),
          messages: [
            ...request.messages,
            first.message,
            ...first.toolCalls.map((call: any) =>
              Message.tool({ id: call.id, name: call.name, result: { condition: "sunny" } }),
            ),
          ],
        })
      }),
  }),
]

const metaSearchScenarios = [
  vendorVariant({
    cassette: "meta-search/searches-and-replays-grounded-responses",
    protocol: "meta-responses",
    provider: { package: "@opencode/ai/providers/meta/responses", modelID: "muse-spark-1.3", settings: metaSettings },
    run: () =>
      Effect.gen(function* () {
        const request = ai.request({
          id: "recorded_meta_search_searches_and_replays_grounded_responses",
          model: metaApi.responses.model("muse-spark-1.3"),
          prompt:
            "Use web search to find NASA's page identifying the first person to walk on the Moon. Answer in one sentence with a source citation.",
          tools: [Meta.webSearch()],
          maxTokens: 2048,
          providerOptions: { reasoningEffort: "low", include: ["reasoning.encrypted_content", "web_search_call.results"] },
        })
        const first = yield* LLMClient.generate(request)
        yield* LLMClient.generate({
          ...request,
          tools: [],
          messages: [
            ...request.messages,
            first.message,
            Message.user("Using the information already found, reply with just that person's surname."),
          ],
        })
      }),
  }),
  vendorVariant({
    cassette: "meta-search/searches-and-replays-grounded-messages",
    protocol: "meta-messages",
    provider: { package: "@opencode/ai/providers/meta/messages", modelID: "muse-spark-1.3", settings: metaSettings },
    run: () =>
      Effect.gen(function* () {
        const request = ai.request({
          id: "recorded_meta_search_searches_and_replays_grounded_messages",
          model: metaApi.messages.model("muse-spark-1.3"),
          prompt:
            "Use web search to find NASA's page identifying the first person to walk on the Moon. Answer in one sentence with a source citation.",
          tools: [Meta.webSearch()],
          maxTokens: 2048,
          providerOptions: { effort: "low" },
        })
        const first = yield* LLMClient.generate(request)
        yield* LLMClient.generate({
          ...request,
          tools: [],
          messages: [
            ...request.messages,
            first.message,
            Message.user("Using the information already found, reply with just that person's surname."),
          ],
        })
      }),
  }),
]

const metaImageScenarios = [
  vendorVariant({
    cassette: "meta-images/replays-a-signed-image-handle-for-a-responses-edit",
    protocol: "meta-responses",
    provider: { package: "@opencode/ai/providers/meta/responses", modelID: "muse-image-1.0", settings: metaSettings },
    run: () =>
      Effect.gen(function* () {
        const request = ai.request({
          id: "recorded_meta_images_replays_a_signed_image_handle_for_a_responses_edit",
          model: metaApi.responses.model("muse-image-1.0"),
          prompt: "A simple flat black square centered on a plain white background. No text.",
          tools: [
            Meta.imageGeneration({
              reasoningStrength: "low",
              enableImageSearch: false,
              enableWebSearch: false,
              enableShell: false,
              size: "1024x1024",
            }),
          ],
          maxTokens: 4096,
        })
        const first = yield* LLMClient.generate(request)
        yield* LLMClient.generate({
          ...request,
          messages: [
            ...request.messages,
            first.message,
            Message.user("Change the square to blue. Keep the white background."),
          ],
        })
      }),
  }),
]

const xaiPdfScenarios = [
  vendorVariant({
    cassette: "pdf/xai-user-input",
    protocol: "xai-responses",
    provider: { package: "@opencode/ai/providers/xai", modelID: "grok-4.5", settings: xaiSettings },
    run: () =>
      LLMClient.generate(
        ai.request({
          id: "recorded_pdf_xai_user_input",
          model: XAI.configure(xaiSettings).responses("grok-4.5"),
          cache: "none",
          maxTokens: 40,
          temperature: 0,
          messages: [
            Message.user([
              { type: "media", media: Media.base64(PDF, "application/pdf"), filename: "verification.pdf" },
              { type: "text", text: pdfPrompt },
            ]),
          ],
        }),
      ),
  }),
  vendorVariant({
    cassette: "pdf/xai-tool-result",
    protocol: "xai-responses",
    provider: { package: "@opencode/ai/providers/xai", modelID: "grok-4.5", settings: xaiSettings },
    run: () =>
      LLMClient.generate(
        ai.request({
          id: "recorded_pdf_xai_tool_result",
          model: XAI.configure(xaiSettings).responses("grok-4.5"),
          system: "Read the PDF returned by the tool and follow the user's response format exactly.",
          cache: "none",
          maxTokens: 40,
          temperature: 0,
          messages: [
            Message.user(pdfPrompt),
            Message.assistant([{ type: "tool-call", id: "call_pdf_1", name: readPdf.name, input: {} }]),
            Message.tool({
              id: "call_pdf_1",
              name: readPdf.name,
              resultType: "content",
              result: [
                { type: "text", text: "PDF read successfully" },
                {
                  type: "file",
                  uri: `data:application/pdf;base64,${PDF}`,
                  mime: "application/pdf",
                  name: "verification.pdf",
                },
              ],
            }),
          ],
          tools: [readPdf],
        }),
      ),
  }),
]

const vendorVariantScenarios = [
  ...alibabaEffortScenarios(
    alibabaApi.chat,
    [undefined, "none", "minimal", "low", "medium", "high", "xhigh", "max"],
    "reasoningEffort",
  ),
  ...alibabaEffortScenarios(alibabaApi.messages, [undefined, "low", "medium", "high", "xhigh", "max"], "effort"),
  ...alibabaEffortScenarios(
    alibabaApi.responses,
    [undefined, "none", "minimal", "low", "medium", "high", "xhigh", "max"],
    "reasoningEffort",
  ),
  ...alibabaThinkingScenarios(alibabaApi.chat),
  ...alibabaThinkingScenarios(alibabaApi.messages),
  ...alibabaThinkingScenarios(alibabaApi.responses),
  ...alibabaCapabilityScenarios(alibabaApi.chat),
  ...alibabaCapabilityScenarios(alibabaApi.messages),
  ...alibabaCapabilityScenarios(alibabaApi.responses),
  ...alibabaLoopScenarios(alibabaApi.chat),
  ...alibabaLoopScenarios(alibabaApi.messages),
  ...alibabaLoopScenarios(alibabaApi.responses),
  ...alibabaResponseExtraScenarios,
  ...metaEffortScenarios(metaApi.chat),
  ...metaEffortScenarios(metaApi.responses),
  ...metaLoopScenarios(metaApi.chat),
  ...metaLoopScenarios(metaApi.responses),
  ...metaMessagesScenarios,
  ...metaSearchScenarios,
  ...metaImageScenarios,
  ...xaiPdfScenarios,
]

const lookupWeather = ToolDefinition.make({
  name: "lookup_weather",
  description: "Look up the current weather for a city",
  inputSchema: { type: "object", properties: { city: { type: "string" } }, required: ["city"] },
})

const nativeVendorScenarios = [
  nativeVendor({
    cassette: "cohere/streams-native-text-and-usage",
    protocol: "cohere-chat",
    provider: {
      package: "@opencode/ai/providers/cohere",
      modelID: "command-a-03-2025",
      settings: { apiKey: "fixture" },
    },
    run: () =>
      LLMClient.generate(
        ai.request({
          model: cohere.model("command-a-03-2025"),
          prompt: "Reply exactly: OK",
          maxTokens: 64,
        }),
      ),
  }),
  nativeVendor({
    cassette: "cohere/streams-native-thinking-with-a-budget",
    protocol: "cohere-chat",
    provider: {
      package: "@opencode/ai/providers/cohere",
      modelID: "command-a-reasoning-08-2025",
      settings: { apiKey: "fixture" },
    },
    run: () =>
      LLMClient.generate(
        ai.request({
          model: cohere.model("command-a-reasoning-08-2025"),
          prompt: "What is 17 times 23? Answer briefly.",
          providerOptions: { thinking: { type: "enabled", tokenBudget: 128 } },
          maxTokens: 2048,
        }),
      ),
  }),
  nativeVendor({
    cassette: "cohere/continues-a-native-tool-call",
    protocol: "cohere-chat",
    provider: {
      package: "@opencode/ai/providers/cohere",
      modelID: "command-a-plus-05-2026",
      settings: { apiKey: "fixture" },
    },
    run: () =>
      runWeatherToolLoop({
        ...goldenWeatherToolLoopRequest({
          id: "cohere-tool-loop",
          model: cohere.model("command-a-plus-05-2026"),
          maxTokens: 2048,
          temperature: false,
        }),
        system: [
          SystemPart.make("Use the get_weather tool exactly once."),
          SystemPart.make("After the tool result, reply exactly: Paris is sunny."),
        ],
      }),
  }),
  nativeVendor({
    cassette: "cohere/streams-compatible-chat-reasoning",
    protocol: "openai-chat",
    provider: {
      package: "@opencode/ai/providers/cohere/chat",
      modelID: "command-a-reasoning-08-2025",
      settings: { apiKey: "fixture" },
    },
    run: () =>
      LLMClient.generate(
        ai.request({
          model: cohere.chat("command-a-reasoning-08-2025"),
          prompt: "What is 17 times 23? Answer briefly.",
          providerOptions: { reasoningEffort: "high" },
          maxTokens: 2048,
        }),
      ),
  }),
  nativeVendor({
    cassette: "google-interactions/streams-text-and-reports-usage",
    protocol: "google-interactions",
    provider: {
      package: "@opencode/ai/providers/google/interactions",
      modelID: "gemini-3.8-flash",
      settings: { apiKey: "fixture" },
    },
    run: () =>
      LLMClient.generate(
        ai.request({
          model: googleInteractions,
          prompt: "Reply with exactly one word: hello",
          maxTokens: 2048,
          providerOptions: { thinkingLevel: "low" },
        }),
      ),
  }),
  nativeVendor({
    cassette: "google-interactions/streams-reasoning-and-retains-thought-signatures",
    protocol: "google-interactions",
    provider: {
      package: "@opencode/ai/providers/google/interactions",
      modelID: "gemini-3.8-flash",
      settings: { apiKey: "fixture" },
    },
    run: () =>
      LLMClient.generate(
        ai.request({
          model: googleInteractions,
          prompt:
            "Find the smallest positive integer that leaves remainder 1 modulo 7, 2 modulo 9, and 3 modulo 11. Explain briefly.",
          maxTokens: 4096,
          providerOptions: { thinkingLevel: "high", thinkingSummaries: "auto" },
        }),
      ),
  }),
  nativeVendor({
    cassette: "google-interactions/replays-native-tool-results-and-signatures-statelessly",
    protocol: "google-interactions",
    provider: {
      package: "@opencode/ai/providers/google/interactions",
      modelID: "gemini-3.8-flash",
      settings: { apiKey: "fixture" },
    },
    run: () =>
      Effect.gen(function* () {
        const request = ai.request({
          model: googleInteractions,
          system: "Use get_weather for weather questions. Answer concisely after receiving the result.",
          prompt: "What is the weather in Paris?",
          tools: [weatherTool],
          toolChoice: { type: "tool", name: weatherTool.name },
          maxTokens: 2048,
          providerOptions: { thinkingLevel: "low", thinkingSummaries: "auto" },
        })
        const first = yield* LLMClient.generate(request)
        const call = first.toolCalls[0]
        if (!call) throw new Error("Missing recorded weather tool call")
        yield* LLMClient.generate(
          ai.request({
            model: googleInteractions,
            system: request.system,
            tools: [weatherTool],
            toolChoice: "none",
            maxTokens: 2048,
            providerOptions: { thinkingLevel: "low", thinkingSummaries: "auto" },
            messages: [
              ...request.messages,
              first.message,
              Message.tool({ id: call.id, name: call.name, result: { temperature: 22, condition: "sunny" } }),
            ],
          }),
        )
      }),
  }),
  nativeVendor({
    cassette: "google-interactions/continues-tool-results-with-previous-interaction-id",
    protocol: "google-interactions",
    provider: {
      package: "@opencode/ai/providers/google/interactions",
      modelID: "gemini-3.8-flash",
      settings: { apiKey: "fixture" },
    },
    run: ({ executor }) =>
      Effect.gen(function* () {
        const request = ai.request({
          model: googleInteractions,
          system: "Use get_weather for weather questions. Answer concisely after receiving the result.",
          prompt: "What is the weather in Paris?",
          tools: [weatherTool],
          toolChoice: "required",
          maxTokens: 2048,
          providerOptions: { thinkingLevel: "low", store: true },
        })
        const first = yield* LLMClient.generate(request)
        const interactionId = first.events.find(LLMEvent.is.finish)?.providerMetadata?.google?.interactionId
        if (typeof interactionId !== "string") throw new Error("Missing recorded interaction id")
        const cleanup = executor
          .execute(
            HttpClientRequest.delete(
              `https://generativelanguage.googleapis.com/v1beta/interactions/${interactionId}`,
            ).pipe(HttpClientRequest.setHeader("x-goog-api-key", "fixture")),
          )
          .pipe(Effect.orDie)
        yield* Effect.gen(function* () {
          const call = first.toolCalls[0]
          if (!call) throw new Error("Missing recorded weather tool call")
          yield* LLMClient.generate(
            ai.request({
              model: googleInteractions,
              system: request.system,
              tools: [weatherTool],
              toolChoice: "none",
              maxTokens: 2048,
              providerOptions: { thinkingLevel: "low", previousInteractionId: interactionId, store: false },
              messages: [
                Message.tool({ id: call.id, name: call.name, result: { temperature: 22, condition: "sunny" } }),
              ],
            }),
          )
        }).pipe(Effect.ensuring(cleanup))
      }),
  }),
  nativeVendor({
    cassette: "google-vertex/streams-text",
    protocol: "gemini",
    provider: {
      package: "@opencode/ai/providers/google-vertex",
      modelID: "gemini-3.5-flash",
      settings: { apiKey: "fixture" },
    },
    run: () =>
      LLMClient.generate(ai.request({ model: googleVertex, prompt: "Reply with exactly one word: hello" })),
  }),
  nativeVendor({
    cassette: "google-vertex/calls-a-tool",
    protocol: "gemini",
    provider: {
      package: "@opencode/ai/providers/google-vertex",
      modelID: "gemini-3.5-flash",
      settings: { apiKey: "fixture" },
    },
    run: () =>
      LLMClient.generate(
        ai.request({
          model: googleVertex,
          prompt: "What is the weather in Paris? Use the lookup_weather tool.",
          tools: [lookupWeather],
        }),
      ),
  }),
  nativeVendor({
    cassette: "google-vertex/continues-after-a-tool-result",
    protocol: "gemini",
    provider: {
      package: "@opencode/ai/providers/google-vertex",
      modelID: "gemini-3.5-flash",
      settings: { apiKey: "fixture" },
    },
    run: () =>
      LLMClient.generate(
        ai.request({
          model: googleVertex,
          messages: [
            Message.user("What is the weather in Paris?"),
            Message.assistant([
              ToolCallPart.make({ id: "call_paris_1", name: "lookup_weather", input: { city: "Paris" } }),
            ]),
            Message.tool({
              id: "call_paris_1",
              name: "lookup_weather",
              result: "18C, light rain",
              resultType: "text",
            }),
          ],
          tools: [lookupWeather],
        }),
      ),
  }),
]

const embeddingValues = ["The invoice was charged twice.", "Refund the duplicate payment."]
const embeddingVariant = (scenario: Scenario) => ({ group: "embeddings", scenario })

const embeddingScenarios = [
  embeddingVariant({
    cassette: "openai-embedding/embeds-text-with-requested-dimensions",
    protocol: "openai-embeddings",
    operation: "embedding.generate",
    provider: {
      package: "@opencode/ai/providers/openai/embeddings",
      modelID: "text-embedding-3-small",
      settings: { apiKey: "fixture" },
    },
    run: (context) => {
      const model = OpenAI.configure({ apiKey: "fixture" }).embedding("text-embedding-3-small")
      const request = ai.embedding.request({ model, values: embeddingValues, dimensions: 256, inputType: "document" })
      return context.operation("embedding.generate", request, ai.embedding.generate(request))
    },
  }),
  embeddingVariant({
    cassette: "cohere-embedding/embeds-text-through-the-compatibility-api",
    protocol: "openai-embeddings",
    operation: "embedding.generate",
    provider: {
      package: "@opencode/ai/providers/cohere/embeddings",
      modelID: "embed-v4.0",
      settings: { apiKey: "fixture" },
    },
    run: (context) => {
      const model = Cohere.configure({ apiKey: "fixture" }).embedding("embed-v4.0")
      const request = ai.embedding.request({ model, values: embeddingValues, dimensions: 256, inputType: "document" })
      return context.operation("embedding.generate", request, ai.embedding.generate(request))
    },
  }),
  embeddingVariant({
    cassette: "google-embedding/embeds-text-through-batchembedcontents",
    protocol: "google-embed-content",
    operation: "embedding.generate",
    provider: {
      package: "@opencode/ai/providers/google/embeddings",
      modelID: "gemini-embedding-2",
      settings: { apiKey: "fixture" },
    },
    run: (context) => {
      const model = Google.configure({ apiKey: "fixture" }).embedding("gemini-embedding-2")
      const request = ai.embedding.request({ model, values: embeddingValues, dimensions: 256, inputType: "document" })
      return context.operation("embedding.generate", request, ai.embedding.generate(request))
    },
  }),
  embeddingVariant({
    cassette: "google-embedding/embeds-an-image-and-text-into-one-vector-each",
    protocol: "google-embed-content",
    operation: "embedding.generate",
    provider: {
      package: "@opencode/ai/providers/google/embeddings",
      modelID: "gemini-embedding-2",
      settings: { apiKey: "fixture" },
    },
    run: (context) =>
      Effect.gen(function* () {
        const image = Media.bytes(
          yield* Effect.promise(() => Bun.file(`${tree}/packages/ai/test/fixtures/images/edit-source.jpg`).bytes()),
          "image/jpeg",
        )
        const model = Google.configure({ apiKey: "fixture" }).embedding("gemini-embedding-2")
        const request = ai.embedding.request({
          model,
          values: [image, "A small square image."],
          dimensions: 128,
        })
        return yield* context.operation("embedding.generate", request, ai.embedding.generate(request))
      }),
  }),
  embeddingVariant({
    cassette: "google-embedding/maps-inputtype-to-a-task-type-for-gemini-embedding-001",
    protocol: "google-embed-content",
    operation: "embedding.generate",
    provider: {
      package: "@opencode/ai/providers/google/embeddings",
      modelID: "gemini-embedding-001",
      settings: { apiKey: "fixture" },
    },
    run: (context) =>
      Effect.gen(function* () {
        const model = Google.configure({ apiKey: "fixture" }).embedding("gemini-embedding-001")
        const query = ai.embedding.request({
          model,
          values: [embeddingValues[0]],
          dimensions: 64,
          inputType: "query",
        })
        yield* context.operation("embedding.generate", query, ai.embedding.generate(query))
        const document = ai.embedding.request({
          model,
          values: [embeddingValues[0]],
          dimensions: 64,
          inputType: "document",
        })
        yield* context.operation("embedding.generate", document, ai.embedding.generate(document))
      }),
  }),
  embeddingVariant({
    cassette: "google-vertex-embedding/embeds-text-through-predict",
    protocol: "google-vertex-predict-embeddings",
    operation: "embedding.generate",
    provider: {
      package: "@opencode/ai/providers/google-vertex/embeddings",
      modelID: "text-embedding-005",
      settings: { apiKey: "fixture" },
    },
    run: (context) => {
      const model = GoogleVertex.configure({ apiKey: "fixture" }).embedding("text-embedding-005")
      const request = ai.embedding.request({ model, values: embeddingValues, dimensions: 256, inputType: "document" })
      return context.operation("embedding.generate", request, ai.embedding.generate(request))
    },
  }),
  embeddingVariant({
    cassette: "google-vertex-embedding/embeds-gemini-embedding-001-one-text-per-predict-request",
    protocol: "google-vertex-predict-embeddings",
    operation: "embedding.generate",
    provider: {
      package: "@opencode/ai/providers/google-vertex/embeddings",
      modelID: "gemini-embedding-001",
      settings: { apiKey: "fixture" },
    },
    run: (context) => {
      const model = GoogleVertex.configure({ apiKey: "fixture" }).embedding("gemini-embedding-001")
      const request = ai.embedding.request({ model, values: embeddingValues, dimensions: 256, inputType: "document" })
      return context.operation("embedding.generate", request, ai.embedding.generate(request))
    },
  }),
]

const scenarios: ReadonlyArray<{ readonly group: string; readonly scenario: Scenario }> = [
  {
    group: "anthropic-messages",
    scenario: {
      cassette: "anthropic-messages/anthropic-opus-4-7-image-tool-result",
      protocol: "anthropic-messages",
      provider: {
        package: "@opencode/ai/providers/anthropic",
        modelID: "claude-opus-4-7",
        settings: { apiKey: "fixture" },
      },
      redact: { allowRequestHeaders: ["anthropic-version"] },
      run: () =>
        runGoldenScenario("image-tool-result", {
          id: "recorded_anthropic_opus_4_7_image_tool_result",
          model: anthropicOpus,
          maxTokens: 40,
          temperature: false,
        }),
    },
  },
  {
    group: "anthropic-messages",
    scenario: {
      cassette: "anthropic-messages-cache/writes-then-reads-cache-control-on-identical-second-call",
      protocol: "anthropic-messages",
      provider: {
        package: "@opencode/ai/providers/anthropic",
        modelID: "claude-haiku-4-5-20251001",
        settings: { apiKey: "fixture" },
      },
      redact: { allowRequestHeaders: ["anthropic-version"] },
      run: () =>
        Effect.gen(function* () {
          yield* LLMClient.generate(anthropicCacheRequest)
          yield* LLMClient.generate(anthropicCacheRequest)
        }),
    },
  },
  {
    group: "anthropic-messages",
    scenario: {
      cassette: "anthropic-messages-cache/keeps-a-long-tool-turn-inside-the-cache-lookback",
      protocol: "anthropic-messages",
      provider: {
        package: "@opencode/ai/providers/anthropic",
        modelID: "claude-haiku-4-5-20251001",
        settings: { apiKey: "fixture" },
      },
      redact: { allowRequestHeaders: ["anthropic-version"] },
      run: () =>
        Effect.gen(function* () {
          yield* LLMClient.generate(longToolTurnRequest)
          yield* LLMClient.generate({
            ...longToolTurnRequest,
            messages: [
              ...longToolTurn,
              Message.assistant("The fixture lookups are complete."),
              Message.user("Reply exactly: OK"),
            ],
          })
        }),
    },
  },
  {
    group: "anthropic-messages",
    scenario: {
      cassette: "anthropic-compatible-messages/minimax-m3-anthropic-compatible-text",
      protocol: "anthropic-messages",
      provider: {
        package: "@opencode/ai/providers/anthropic-compatible",
        modelID: "MiniMax-M3",
        settings: { apiKey: "fixture", baseURL: "https://api.minimax.io/anthropic/v1", provider: "minimax" },
      },
      redact: { allowRequestHeaders: ["anthropic-version"] },
      run: () =>
        runGoldenScenario("text", {
          id: "recorded_minimax_m3_anthropic_compatible_text",
          model: minimax,
        }),
    },
  },
  {
    group: "anthropic-messages",
    scenario: {
      cassette: "anthropic-compatible-messages/minimax-m3-anthropic-compatible-tool-call",
      protocol: "anthropic-messages",
      provider: {
        package: "@opencode/ai/providers/anthropic-compatible",
        modelID: "MiniMax-M3",
        settings: { apiKey: "fixture", baseURL: "https://api.minimax.io/anthropic/v1", provider: "minimax" },
      },
      redact: { allowRequestHeaders: ["anthropic-version"] },
      run: () =>
        runGoldenScenario("tool-call", {
          id: "recorded_minimax_m3_anthropic_compatible_tool_call",
          model: minimax,
        }),
    },
  },
  {
    group: "anthropic-messages",
    scenario: {
      cassette: "anthropic-compatible-messages/minimax-m3-anthropic-compatible-tool-loop",
      protocol: "anthropic-messages",
      provider: {
        package: "@opencode/ai/providers/anthropic-compatible",
        modelID: "MiniMax-M3",
        settings: { apiKey: "fixture", baseURL: "https://api.minimax.io/anthropic/v1", provider: "minimax" },
      },
      redact: { allowRequestHeaders: ["anthropic-version"] },
      run: () =>
        runGoldenScenario("tool-loop", {
          id: "recorded_minimax_m3_anthropic_compatible_tool_loop",
          model: minimax,
        }),
    },
  },
  {
    group: "anthropic-messages",
    scenario: {
      cassette: "pdf/anthropic-user-input",
      protocol: "anthropic-messages",
      provider: {
        package: "@opencode/ai/providers/anthropic",
        modelID: "claude-haiku-4-5-20251001",
        settings: { apiKey: "fixture" },
      },
      run: () =>
        LLMClient.generate(
          ai.request({
            id: "recorded_pdf_anthropic_user_input",
            model: anthropicHaiku,
            cache: "none",
            maxTokens: 40,
            temperature: 0,
            messages: [
              Message.user([
                { type: "media", media: Media.base64(PDF, "application/pdf"), filename: "verification.pdf" },
                { type: "text", text: pdfPrompt },
              ]),
            ],
          }),
        ),
    },
  },
  {
    group: "anthropic-messages",
    scenario: {
      cassette: "pdf/anthropic-tool-result",
      protocol: "anthropic-messages",
      provider: {
        package: "@opencode/ai/providers/anthropic",
        modelID: "claude-haiku-4-5-20251001",
        settings: { apiKey: "fixture" },
      },
      run: () =>
        LLMClient.generate(
          ai.request({
            id: "recorded_pdf_anthropic_tool_result",
            model: anthropicHaiku,
            system: "Read the PDF returned by the tool and follow the user's response format exactly.",
            cache: "none",
            maxTokens: 40,
            temperature: 0,
            messages: [
              Message.user(pdfPrompt),
              Message.assistant([{ type: "tool-call", id: "call_pdf_1", name: readPdf.name, input: {} }]),
              Message.tool({
                id: "call_pdf_1",
                name: readPdf.name,
                resultType: "content",
                result: [
                  { type: "text", text: "PDF read successfully" },
                  {
                    type: "file",
                    uri: `data:application/pdf;base64,${PDF}`,
                    mime: "application/pdf",
                    name: "verification.pdf",
                  },
                ],
              }),
            ],
            tools: [readPdf],
          }),
        ),
    },
  },
  {
    group: "openai-chat",
    scenario: {
      cassette: "openai-compatible-chat/gemini-parallel-tool-signatures",
      protocol: "openai-chat",
      provider: {
        package: "@opencode/ai/providers/openai-compatible",
        modelID: "gemini-3.8-flash",
        settings: {
          provider: "google",
          baseURL: "https://generativelanguage.googleapis.com/v1beta/openai",
          apiKey: "fixture",
        },
      },
      run: () =>
        Effect.gen(function* () {
          const tools = [weatherRuntimeTool]
          const request = ai.request({
            model: geminiOpenAIChat,
            system: "Call get_weather for every requested city in parallel, then answer in one short sentence.",
            prompt: "What is the weather in Paris and in Tokyo?",
            tools,
            cache: "none",
          })
          const first = yield* LLMClient.generate(request)
          const calls = first.events.filter(LLMEvent.is.toolCall)
          const results: ReadonlyArray<any> = yield* Effect.forEach(calls, (call: any) =>
            ToolRuntime.dispatch(tools, call),
          )
          yield* LLMClient.generate({
            ...request,
            messages: [
              ...request.messages,
              first.message,
              ...calls.map((call: any, index: number) =>
                Message.tool({ id: call.id, name: call.name, result: results[index]!.result }),
              ),
            ],
          })
        }),
    },
  },
  {
    group: "openai-chat",
    scenario: {
      cassette: "openai-compatible-chat/togetherai-streams-text",
      protocol: "openai-chat",
      provider: {
        package: "@opencode/ai/providers/togetherai",
        modelID: "meta-llama/Llama-3.3-70B-Instruct-Turbo",
        settings: { apiKey: "fixture" },
      },
      run: () =>
        runGoldenScenario("text", {
          id: "recorded_togetherai_llama_3_3_70b_text",
          prompt: "Reply with exactly: Hello!",
          model: together,
          maxTokens: 20,
        }),
    },
  },
  {
    group: "openai-chat",
    scenario: {
      cassette: "openai-compatible-chat/togetherai-streams-tool-call",
      protocol: "openai-chat",
      provider: {
        package: "@opencode/ai/providers/togetherai",
        modelID: "meta-llama/Llama-3.3-70B-Instruct-Turbo",
        settings: { apiKey: "fixture" },
      },
      run: () =>
        runGoldenScenario("tool-call", {
          id: "recorded_togetherai_llama_3_3_70b_tool_call",
          model: together,
        }),
    },
  },
  {
    group: "openai-chat",
    scenario: {
      cassette: "cerebras-chat/cerebras-gpt-oss-120b-text",
      protocol: "openai-chat",
      provider: {
        package: "@opencode/ai/providers/cerebras",
        modelID: "gpt-oss-120b",
        settings: { apiKey: "fixture" },
      },
      run: () =>
        runGoldenScenario("text", {
          id: "recorded_cerebras_gpt_oss_120b_text",
          model: cerebras,
          maxTokens: 256,
          temperature: false,
        }),
    },
  },
  {
    group: "openai-chat",
    scenario: {
      cassette: "cerebras-chat/cerebras-gpt-oss-120b-tool-call",
      protocol: "openai-chat",
      provider: {
        package: "@opencode/ai/providers/cerebras",
        modelID: "gpt-oss-120b",
        settings: { apiKey: "fixture" },
      },
      run: () =>
        runGoldenScenario("tool-call", {
          id: "recorded_cerebras_gpt_oss_120b_tool_call",
          model: cerebras,
          maxTokens: 512,
          temperature: false,
        }),
    },
  },
  {
    group: "openai-chat",
    scenario: {
      cassette: "cerebras-chat/cerebras-gpt-oss-120b-tool-loop",
      protocol: "openai-chat",
      provider: {
        package: "@opencode/ai/providers/cerebras",
        modelID: "gpt-oss-120b",
        settings: { apiKey: "fixture" },
      },
      run: () =>
        runGoldenScenario("tool-loop", {
          id: "recorded_cerebras_gpt_oss_120b_tool_loop",
          model: cerebras,
          maxTokens: 512,
          temperature: false,
        }),
    },
  },
  {
    group: "openai-chat",
    scenario: {
      cassette: "deepinfra-chat/deepinfra-llama-3-3-70b-text",
      protocol: "openai-chat",
      provider: {
        package: "@opencode/ai/providers/deepinfra",
        modelID: "meta-llama/Llama-3.3-70B-Instruct-Turbo",
        settings: { apiKey: "fixture" },
      },
      run: () =>
        runGoldenScenario("text", {
          id: "recorded_deepinfra_llama_3_3_70b_text",
          model: deepinfra,
        }),
    },
  },
  {
    group: "openai-chat",
    scenario: {
      cassette: "deepinfra-chat/deepinfra-llama-3-3-70b-tool-call",
      protocol: "openai-chat",
      provider: {
        package: "@opencode/ai/providers/deepinfra",
        modelID: "meta-llama/Llama-3.3-70B-Instruct-Turbo",
        settings: { apiKey: "fixture" },
      },
      run: () =>
        runGoldenScenario("tool-call", {
          id: "recorded_deepinfra_llama_3_3_70b_tool_call",
          model: deepinfra,
        }),
    },
  },
  {
    group: "openai-chat",
    scenario: {
      cassette: "deepinfra-chat/deepinfra-llama-3-3-70b-tool-loop",
      protocol: "openai-chat",
      provider: {
        package: "@opencode/ai/providers/deepinfra",
        modelID: "meta-llama/Llama-3.3-70B-Instruct-Turbo",
        settings: { apiKey: "fixture" },
      },
      run: () =>
        runGoldenScenario("tool-loop", {
          id: "recorded_deepinfra_llama_3_3_70b_tool_loop",
          model: deepinfra,
        }),
    },
  },
  {
    group: "openai-chat",
    scenario: {
      cassette: "vercel-ai-gateway-reasoning",
      protocol: "openai-chat",
      provider: {
        package: "@opencode/ai/providers/openai-compatible",
        modelID: "anthropic/claude-sonnet-4.6",
        settings: {
          provider: "vercel-ai-gateway",
          baseURL: "https://ai-gateway.vercel.sh/v1",
          apiKey: "fixture",
          body: { reasoning: { enabled: true, max_tokens: 1024 } },
        },
      },
      run: () =>
        LLMClient.generate(
          ai.request({
            model: vercel,
            system: "Think through the arithmetic, then reply with only the final integer.",
            prompt: "What is 173 multiplied by 219?",
            maxTokens: 1536,
            temperature: 0,
          }),
        ),
    },
  },
  {
    group: "openai-chat",
    scenario: {
      cassette: "vercel-ai-gateway-reasoning-tool-loop",
      protocol: "openai-chat",
      provider: {
        package: "@opencode/ai/providers/openai-compatible",
        modelID: "anthropic/claude-sonnet-4.6",
        settings: {
          provider: "vercel-ai-gateway",
          baseURL: "https://ai-gateway.vercel.sh/v1",
          apiKey: "fixture",
          body: { reasoning: { enabled: true, max_tokens: 1024 } },
        },
      },
      run: () =>
        runWeatherToolLoop(
          goldenWeatherToolLoopRequest({
            id: "vercel-ai-gateway-reasoning-tool-loop",
            model: vercel,
            maxTokens: 1536,
            temperature: false,
          }),
        ),
    },
  },
  {
    group: "open-responses",
    scenario: {
      cassette: "openai-responses/openai-responses-gpt-5-5-reasoning",
      protocol: "openai-responses",
      provider: {
        package: "@opencode/ai/providers/openai/responses",
        modelID: "gpt-5.5",
        settings: { apiKey: "fixture" },
      },
      run: () =>
        runGoldenScenario("reasoning", {
          id: "recorded_openai_responses_gpt_5_5_reasoning",
          model: openaiResponses,
          temperature: false,
        }),
    },
  },
  {
    group: "open-responses",
    scenario: {
      cassette: "openai-responses/openai-responses-gpt-5-5-reasoning-continuation",
      protocol: "openai-responses",
      provider: {
        package: "@opencode/ai/providers/openai/responses",
        modelID: "gpt-5.5",
        settings: { apiKey: "fixture" },
      },
      run: () =>
        runGoldenScenario("reasoning-continuation", {
          id: "recorded_openai_responses_gpt_5_5_reasoning_continuation",
          model: openaiResponses,
          temperature: false,
        }),
    },
  },
  {
    group: "open-responses",
    scenario: {
      cassette: "openai-responses/openai-responses-gpt-5-5-tool-loop",
      protocol: "openai-responses",
      provider: {
        package: "@opencode/ai/providers/openai/responses",
        modelID: "gpt-5.5",
        settings: { apiKey: "fixture" },
      },
      run: () =>
        runGoldenScenario("tool-loop", {
          id: "recorded_openai_responses_gpt_5_5_tool_loop",
          model: openaiResponses,
          temperature: false,
        }),
    },
  },
  {
    group: "open-responses",
    scenario: {
      cassette: "openai-responses/openai-responses-gpt-5-5-image-tool-result",
      protocol: "openai-responses",
      provider: {
        package: "@opencode/ai/providers/openai/responses",
        modelID: "gpt-5.5",
        settings: { apiKey: "fixture" },
      },
      run: () =>
        runGoldenScenario("image-tool-result", {
          id: "recorded_openai_responses_gpt_5_5_image_tool_result",
          model: openaiResponses,
          maxTokens: 40,
          temperature: false,
        }),
    },
  },
  {
    group: "open-responses",
    scenario: {
      cassette: "openai-responses-cache/reports-cached-tokens-on-identical-second-call",
      protocol: "openai-responses",
      provider: {
        package: "@opencode/ai/providers/openai/responses",
        modelID: "gpt-4.1-mini",
        settings: { apiKey: "fixture" },
      },
      run: () =>
        Effect.gen(function* () {
          yield* LLMClient.generate(openaiCacheRequest)
          yield* LLMClient.generate(openaiCacheRequest)
        }),
    },
  },
  {
    group: "open-responses",
    scenario: {
      cassette: "openai-responses-cache/caches-the-system-prompt-on-gpt-6-2-sol",
      protocol: "openai-responses",
      provider: {
        package: "@opencode/ai/providers/openai/responses",
        modelID: "gpt-6.2-sol",
        settings: { apiKey: "fixture" },
      },
      run: () =>
        Effect.gen(function* () {
          yield* LLMClient.generate(openaiFrontierRequest)
          yield* LLMClient.generate(openaiFrontierRequest)
        }),
    },
  },
  {
    group: "open-responses",
    scenario: {
      cassette: "openai-responses-images/generates-and-edits-an-image-with-the-hosted-tool",
      protocol: "openai-responses",
      provider: {
        package: "@opencode/ai/providers/openai/responses",
        modelID: "gpt-5-mini",
        settings: { apiKey: "fixture" },
      },
      run: () =>
        Effect.gen(function* () {
          const initial = Message.user("Generate a simple flat black triangle centered on a plain white background.")
          const tools = [
            OpenAI.imageGeneration({
              action: "auto",
              quality: "low",
              size: "1024x1024",
              outputFormat: "jpeg",
              outputCompression: 10,
              partialImages: 0,
            }),
          ]
          const response = yield* ai.generate(
            ai.request({
              model: openai.responses("gpt-5-mini"),
              messages: [initial],
              tools,
              toolChoice: "image_generation",
            }),
          )
          yield* ai.generate(
            ai.request({
              model: openai.responses("gpt-5-mini"),
              messages: [initial, response.message, Message.user("Now make the triangle blue.")],
              tools,
              toolChoice: "image_generation",
            }),
          )
        }),
    },
  },
  {
    group: "open-responses",
    scenario: {
      cassette: "pdf/openai-user-input",
      protocol: "openai-responses",
      provider: {
        package: "@opencode/ai/providers/openai/responses",
        modelID: "gpt-4o-mini",
        settings: { apiKey: "fixture" },
      },
      run: () =>
        LLMClient.generate(
          ai.request({
            id: "recorded_pdf_openai_user_input",
            model: openai.responses("gpt-4o-mini"),
            cache: "none",
            maxTokens: 40,
            temperature: 0,
            messages: [
              Message.user([
                { type: "media", media: Media.base64(PDF, "application/pdf"), filename: "verification.pdf" },
                { type: "text", text: pdfPrompt },
              ]),
            ],
          }),
        ),
    },
  },
  {
    group: "open-responses",
    scenario: {
      cassette: "pdf/openai-tool-result",
      protocol: "openai-responses",
      provider: {
        package: "@opencode/ai/providers/openai/responses",
        modelID: "gpt-4o-mini",
        settings: { apiKey: "fixture" },
      },
      run: () =>
        LLMClient.generate(
          ai.request({
            id: "recorded_pdf_openai_tool_result",
            model: openai.responses("gpt-4o-mini"),
            system: "Read the PDF returned by the tool and follow the user's response format exactly.",
            cache: "none",
            maxTokens: 40,
            temperature: 0,
            messages: [
              Message.user(pdfPrompt),
              Message.assistant([{ type: "tool-call", id: "call_pdf_1", name: readPdf.name, input: {} }]),
              Message.tool({
                id: "call_pdf_1",
                name: readPdf.name,
                resultType: "content",
                result: [
                  { type: "text", text: "PDF read successfully" },
                  {
                    type: "file",
                    uri: `data:application/pdf;base64,${PDF}`,
                    mime: "application/pdf",
                    name: "verification.pdf",
                  },
                ],
              }),
            ],
            tools: [readPdf],
          }),
        ),
    },
  },
  {
    group: "open-responses",
    scenario: {
      cassette: "venice-responses/streams-text",
      protocol: "open-responses",
      provider: {
        package: "@opencode/ai/providers/venice/responses",
        modelID: "openai-gpt-54-mini",
        settings: { apiKey: "fixture" },
      },
      run: () => LLMClient.generate(ai.request({ model: venice, prompt: "Reply exactly: OK", maxTokens: 64 })),
    },
  },
  {
    group: "open-responses",
    scenario: {
      cassette: "venice-responses/continues-a-tool-call",
      protocol: "open-responses",
      provider: {
        package: "@opencode/ai/providers/venice/responses",
        modelID: "openai-gpt-54-mini",
        settings: { apiKey: "fixture" },
      },
      run: () =>
        runWeatherToolLoop(
          weatherToolLoopRequest({
            id: "venice-responses-tool-loop",
            model: venice,
            maxTokens: 256,
            temperature: false,
          }),
        ),
    },
  },
  {
    group: "open-responses",
    scenario: {
      cassette: "venice-responses/calls-a-freeform-text-tool",
      protocol: "open-responses",
      provider: {
        package: "@opencode/ai/providers/venice/responses",
        modelID: "openai-gpt-54-mini",
        settings: { apiKey: "fixture" },
      },
      run: () =>
        LLMClient.generate(
          ai.request({
            model: venice,
            prompt: "Use the run tool to execute exactly this code: print(1)",
            tools: freeformRun,
            toolChoice: "required",
            maxTokens: 256,
          }),
        ),
    },
  },
  {
    group: "openai-responses-websocket",
    scenario: {
      cassette: "openai-responses-websocket/continues-a-tool-call-over-one-socket",
      protocol: "openai-responses",
      auth: { type: "bearer", value: "fixture" },
      descriptorOnly: "the stream selects the OpenAI Responses channel per call",
      transport: "websocket",
      run: ({ channel }) =>
        Effect.gen(function* () {
          const request = ai.request({
            id: "recorded_openai_responses_websocket_tool",
            model: openaiResponses,
            system: "Call get_weather once, then reply exactly: Paris is sunny.",
            prompt: "What is the weather in Paris?",
            tools: [weatherTool],
            maxTokens: 50,
            cache: "none",
          })
          const first = yield* LLMClient.generate(request, { webSocket: channel.executor })
          const call = first.toolCalls[0]
          if (!call) yield* Effect.die("Expected get_weather tool call")
          const result = yield* ToolRuntime.dispatch([weatherRuntimeTool], call)
          yield* LLMClient.generate(
            {
              ...request,
              messages: [
                ...request.messages,
                first.message,
                Message.tool({ id: call.id, name: call.name, result: result.result }),
              ],
            },
            { webSocket: channel.executor },
          )
        }),
    },
  },
  {
    group: "openai-responses-websocket",
    scenario: {
      cassette: "openai-responses-websocket/reconstructs-full-context-after-reconnect",
      protocol: "openai-responses",
      auth: { type: "bearer", value: "fixture" },
      descriptorOnly: "the stream selects the OpenAI Responses channel per call",
      transport: "websocket",
      run: ({ channel }) =>
        Effect.gen(function* () {
          const request = ai.request({
            id: "recorded_openai_responses_websocket_reconnect",
            model: openaiResponses,
            system: "Follow the user's exact reply instruction.",
            prompt: "Reply exactly: Alpha.",
            maxTokens: 30,
            cache: "none",
          })
          const first = yield* LLMClient.generate(request, { webSocket: channel.executor })
          yield* channel.reconnect()
          yield* LLMClient.generate(
            { ...request, messages: [...request.messages, first.message, Message.user("Reply exactly: Beta.")] },
            { webSocket: channel.executor },
          )
        }),
    },
  },
  {
    group: "openai-responses-websocket",
    scenario: {
      cassette: "openai-responses-websocket/recovers-from-explicit-continuation-rejection",
      protocol: "openai-responses",
      auth: { type: "bearer", value: "fixture" },
      descriptorOnly: "the stream selects the OpenAI Responses channel per call",
      transport: "websocket",
      run: ({ channel }) =>
        Effect.gen(function* () {
          const request = ai.request({
            id: "recorded_openai_responses_websocket_rejection",
            model: openaiResponses,
            system: "Follow the user's exact reply instruction.",
            prompt: "Reply exactly: Ready.",
            maxTokens: 30,
            cache: "none",
          })
          const first = yield* LLMClient.generate(request, { webSocket: channel.executor })
          const continuation = {
            ...request,
            messages: [...request.messages, first.message, Message.user("Reply exactly: Recovered.")],
          }
          yield* channel.reconnect(true)
          yield* LLMClient.generate(continuation, { webSocket: channel.executor }).pipe(Effect.flip)
          yield* LLMClient.generate(continuation, { webSocket: channel.executor })
        }),
    },
  },
  {
    group: "gemini",
    scenario: {
      cassette: "gemini/gemini-2-5-flash-image",
      protocol: "gemini",
      provider: {
        package: "@opencode/ai/providers/google",
        modelID: "gemini-2.5-flash",
        settings: { apiKey: "fixture" },
      },
      run: () =>
        runGoldenScenario("image", {
          id: "recorded_gemini_2_5_flash_image",
          model: geminiFlash,
          maxTokens: 160,
        }),
    },
  },
  {
    group: "gemini",
    scenario: {
      cassette: "gemini-cache/reports-cachedcontenttokencount-on-identical-second-call",
      protocol: "gemini",
      provider: {
        package: "@opencode/ai/providers/google",
        modelID: "gemini-2.5-flash",
        settings: { apiKey: "fixture" },
      },
      run: () =>
        Effect.gen(function* () {
          yield* LLMClient.generate(geminiCacheRequest)
          yield* LLMClient.generate(geminiCacheRequest)
        }),
    },
  },
  {
    group: "gemini",
    scenario: {
      cassette: "pdf/gemini-user-input",
      protocol: "gemini",
      provider: {
        package: "@opencode/ai/providers/google",
        modelID: "gemini-3.5-flash",
        settings: { apiKey: "fixture" },
      },
      run: () =>
        LLMClient.generate(
          ai.request({
            id: "recorded_pdf_gemini_user_input",
            model: pdfGemini,
            cache: "none",
            maxTokens: 256,
            temperature: 0,
            messages: [
              Message.user([
                { type: "media", media: Media.base64(PDF, "application/pdf"), filename: "verification.pdf" },
                { type: "text", text: pdfPrompt },
              ]),
            ],
          }),
        ),
    },
  },
  {
    group: "gemini",
    scenario: {
      cassette: "pdf/gemini-tool-result",
      protocol: "gemini",
      provider: {
        package: "@opencode/ai/providers/google",
        modelID: "gemini-3.5-flash",
        settings: { apiKey: "fixture" },
      },
      run: () =>
        runTools({
          request: ai.request({
            id: "recorded_pdf_gemini_tool_result",
            model: pdfGemini,
            system:
              "Call read_pdf exactly once with path verification.pdf, then reply only with the verification code from its PDF.",
            prompt: "Use read_pdf with path verification.pdf and return the verification code.",
            cache: "none",
            maxTokens: 256,
            temperature: 0,
          }),
          tools: [readPdfRuntime],
        }).pipe(Stream.runCollect),
    },
  },
  {
    group: "bedrock-converse",
    scenario: {
      cassette: "bedrock-converse/streams-text",
      protocol: "bedrock-converse",
      provider: {
        package: "@opencode/ai/providers/amazon-bedrock",
        modelID: "us.amazon.nova-micro-v1:0",
        settings: { credentials: { region: "us-east-1", accessKeyId: "fixture", secretAccessKey: "fixture" } },
      },
      run: () =>
        LLMClient.generate(
          ai.request({
            id: "recorded_bedrock_text",
            model: bedrockConversation,
            system: "Reply with the single word 'Hello'.",
            prompt: "Say hello.",
            cache: "none",
            maxTokens: 16,
            temperature: 0,
          }),
        ),
    },
  },
  {
    group: "bedrock-converse",
    scenario: {
      cassette: "bedrock-converse/streams-a-tool-call",
      protocol: "bedrock-converse",
      provider: {
        package: "@opencode/ai/providers/amazon-bedrock",
        modelID: "us.amazon.nova-micro-v1:0",
        settings: { credentials: { region: "us-east-1", accessKeyId: "fixture", secretAccessKey: "fixture" } },
      },
      run: () =>
        LLMClient.generate(
          ai.request({
            id: "recorded_bedrock_tool_call",
            model: bedrockConversation,
            system: "Call tools exactly as requested.",
            prompt: "Call get_weather with city exactly Paris.",
            tools: [weatherTool],
            toolChoice: ToolChoice.make(weatherTool),
            cache: "none",
            maxTokens: 80,
            temperature: 0,
          }),
        ),
    },
  },
  {
    group: "bedrock-converse",
    scenario: {
      cassette: "bedrock-converse/drives-a-tool-loop",
      protocol: "bedrock-converse",
      provider: {
        package: "@opencode/ai/providers/amazon-bedrock",
        modelID: "us.amazon.nova-micro-v1:0",
        settings: { credentials: { region: "us-east-1", accessKeyId: "fixture", secretAccessKey: "fixture" } },
      },
      run: () =>
        runWeatherToolLoop(
          weatherToolLoopRequest({
            id: "recorded_bedrock_tool_loop",
            model: bedrockConversation,
          }),
        ),
    },
  },
  {
    group: "bedrock-converse",
    scenario: {
      cassette: "bedrock-converse/continues-after-parallel-tool-results",
      protocol: "bedrock-converse",
      provider: {
        package: "@opencode/ai/providers/amazon-bedrock",
        modelID: "us.amazon.nova-micro-v1:0",
        settings: { credentials: { region: "us-east-1", accessKeyId: "fixture", secretAccessKey: "fixture" } },
      },
      run: () =>
        LLMClient.generate(
          ai.request({
            id: "recorded_bedrock_parallel_tool_results",
            model: bedrockConversation,
            system: "After receiving both tool results, reply exactly: Paris is sunny; London is rainy.",
            messages: [
              Message.user("Compare the weather in Paris and London."),
              Message.assistant([
                ToolCallPart.make({ id: "weather_paris", name: weatherTool.name, input: { city: "Paris" } }),
                ToolCallPart.make({ id: "weather_london", name: weatherTool.name, input: { city: "London" } }),
              ]),
              Message.tool({
                id: "weather_paris",
                name: weatherTool.name,
                result: { temperature: 22, condition: "sunny" },
              }),
              Message.tool({
                id: "weather_london",
                name: weatherTool.name,
                result: { temperature: 14, condition: "rainy" },
              }),
            ],
            tools: [weatherTool],
            cache: "none",
            maxTokens: 40,
            temperature: 0,
          }),
        ),
    },
  },
  {
    group: "bedrock-converse",
    scenario: {
      cassette: "bedrock-converse-cache/writes-then-reads-cachepoint-on-identical-second-call",
      protocol: "bedrock-converse",
      provider: {
        package: "@opencode/ai/providers/amazon-bedrock",
        modelID: "us.anthropic.claude-haiku-4-5-20251001-v1:0",
        settings: { apiKey: "fixture", region: "us-east-1" },
      },
      run: () =>
        Effect.gen(function* () {
          yield* LLMClient.generate(bedrockCacheRequest)
          yield* LLMClient.generate(bedrockCacheRequest)
        }),
    },
  },
  {
    group: "bedrock-converse",
    scenario: {
      cassette: "bedrock-converse-options/constrains-claude-output-to-a-json-schema",
      protocol: "bedrock-converse",
      provider: {
        package: "@opencode/ai/providers/amazon-bedrock",
        modelID: "us.anthropic.claude-sonnet-4-6",
        settings: {
          apiKey: "fixture",
          region: "us-east-1",
          outputFormat: {
            type: "json_schema",
            name: "answer",
            schema: {
              type: "object",
              properties: { product: { type: "integer" } },
              required: ["product"],
              additionalProperties: false,
            },
          },
        },
      },
      run: () =>
        LLMClient.generate(
          ai.request({
            id: "recorded_bedrock_options_output_format",
            model: bedrockOptions("us.anthropic.claude-sonnet-4-6", {
              outputFormat: {
                type: "json_schema",
                name: "answer",
                schema: {
                  type: "object",
                  properties: { product: { type: "integer" } },
                  required: ["product"],
                  additionalProperties: false,
                },
              },
            }),
            prompt: "What is 17*23?",
            cache: "none",
            maxTokens: 200,
          }),
        ),
    },
  },
  {
    group: "bedrock-converse",
    scenario: {
      cassette: "bedrock-converse-options/sends-claude-effort-with-adaptive-thinking-service-tier-and-request-metadata",
      protocol: "bedrock-converse",
      provider: {
        package: "@opencode/ai/providers/amazon-bedrock",
        modelID: "us.anthropic.claude-sonnet-4-6",
        settings: {
          apiKey: "fixture",
          region: "us-east-1",
          effort: "high",
          thinking: { type: "adaptive", display: "summarized" },
          serviceTier: "default",
          requestMetadata: { suite: "opencode-ai" },
        },
      },
      run: () =>
        LLMClient.generate(
          ai.request({
            id: "recorded_bedrock_options_claude",
            model: bedrockOptions("us.anthropic.claude-sonnet-4-6", {
              effort: "high",
              thinking: { type: "adaptive", display: "summarized" },
              serviceTier: "default",
              requestMetadata: { suite: "opencode-ai" },
            }),
            prompt: "How many primes are below 60? Reply with the number only.",
            cache: "none",
            maxTokens: 4_000,
          }),
        ),
    },
  },
  {
    group: "bedrock-converse",
    scenario: {
      cassette: "bedrock-converse-options/sends-gpt-oss-reasoning-effort-with-the-priority-tier",
      protocol: "bedrock-converse",
      provider: {
        package: "@opencode/ai/providers/amazon-bedrock",
        modelID: "openai.gpt-oss-120b-1:0",
        settings: {
          apiKey: "fixture",
          region: "us-east-1",
          effort: "low",
          serviceTier: "priority",
          requestMetadata: { suite: "opencode-ai" },
        },
      },
      run: () =>
        LLMClient.generate(
          ai.request({
            id: "recorded_bedrock_options_gpt_oss",
            model: bedrockOptions("openai.gpt-oss-120b-1:0", {
              effort: "low",
              serviceTier: "priority",
              requestMetadata: { suite: "opencode-ai" },
            }),
            prompt: "What is 17*23? Reply with the number only.",
            cache: "none",
            maxTokens: 1_000,
          }),
        ),
    },
  },
  {
    group: "bedrock-converse",
    scenario: {
      cassette: "bedrock-converse-options/sends-nova-2-reasoning-effort",
      protocol: "bedrock-converse",
      provider: {
        package: "@opencode/ai/providers/amazon-bedrock",
        modelID: "us.amazon.nova-2-lite-v1:0",
        settings: { apiKey: "fixture", region: "us-east-1", effort: "low" },
      },
      run: () =>
        LLMClient.generate(
          ai.request({
            id: "recorded_bedrock_options_nova",
            model: bedrockOptions("us.amazon.nova-2-lite-v1:0", { effort: "low" }),
            prompt: "What is 17*23? Reply with the number only.",
            cache: "none",
            maxTokens: 1_000,
          }),
        ),
    },
  },
  {
    group: "bedrock-converse",
    scenario: {
      cassette: "bedrock-converse-thinking-binding/accepts-the-default-binding-with-no-thinking-configured",
      protocol: "bedrock-converse",
      provider: {
        package: "@opencode/ai/providers/amazon-bedrock",
        modelID: "global.anthropic.claude-opus-5-5",
        settings: { apiKey: "fixture", region: "us-east-1" },
      },
      run: () =>
        LLMClient.generate(
          ai.request({
            id: "recorded_bedrock_thinking_binding_default",
            model: AmazonBedrock.model("global.anthropic.claude-opus-5-5", {
              apiKey: "fixture",
              region: "us-east-1",
            }),
            system: "Reply with the single word 'Hello'.",
            prompt: "Say hello.",
            cache: "none",
            maxTokens: 1_024,
          }),
        ),
    },
  },
  {
    group: "bedrock-converse",
    scenario: {
      cassette: "bedrock-converse-thinking-binding/keeps-a-session-working-after-the-system-prompt-changes",
      protocol: "bedrock-converse",
      auth: { type: "bearer", value: "fixture" },
      descriptorOnly: "the steps use different bedrock thinking settings",
      run: () =>
        Effect.gen(function* () {
          const first = ai.request({
            id: "recorded_bedrock_thinking_binding_first",
            model: bedrockThinking(),
            system: "You are a concise assistant.",
            prompt:
              "How many positive integers below 5000 have exactly 12 positive divisors? Reply with the number only.",
            cache: "none",
            maxTokens: 12_000,
          })
          const turn = yield* LLMClient.generate(first)
          const next = {
            ...first,
            id: "recorded_bedrock_thinking_binding_next",
            system: [{ type: "text", text: "You are a concise assistant. Today is Monday." }],
            messages: [
              ...first.messages,
              Message.assistant(turn.message.content),
              Message.user("Now double it. Reply with the number only."),
            ],
          }
          yield* LLMClient.generate(next)
          yield* LLMClient.generate({
            ...next,
            id: "recorded_bedrock_thinking_binding_error",
            model: bedrockThinking({ prefix_mismatch_behavior: "error" }),
          }).pipe(Effect.flip)
        }),
    },
  },
  {
    group: "bedrock-converse",
    scenario: {
      cassette: "pdf/bedrock-user-input",
      protocol: "bedrock-converse",
      provider: {
        package: "@opencode/ai/providers/amazon-bedrock",
        modelID: "us.anthropic.claude-haiku-4-5-20251001-v1:0",
        settings: { apiKey: "fixture", region: "us-east-1" },
      },
      run: () =>
        LLMClient.generate(
          ai.request({
            id: "recorded_pdf_bedrock_user_input",
            model: bedrockPdf,
            cache: "none",
            maxTokens: 40,
            temperature: 0,
            messages: [
              Message.user([
                { type: "media", media: Media.base64(PDF, "application/pdf"), filename: "verification" },
                { type: "text", text: pdfPrompt },
              ]),
            ],
          }),
        ),
    },
  },
  {
    group: "bedrock-converse",
    scenario: {
      cassette: "pdf/bedrock-tool-result",
      protocol: "bedrock-converse",
      provider: {
        package: "@opencode/ai/providers/amazon-bedrock",
        modelID: "us.anthropic.claude-haiku-4-5-20251001-v1:0",
        settings: { apiKey: "fixture", region: "us-east-1" },
      },
      run: () =>
        LLMClient.generate(
          ai.request({
            id: "recorded_pdf_bedrock_tool_result",
            model: bedrockPdf,
            system: "Read the PDF returned by the tool and follow the user's response format exactly.",
            cache: "none",
            maxTokens: 40,
            temperature: 0,
            messages: [
              Message.user(pdfPrompt),
              Message.assistant([{ type: "tool-call", id: "call_pdf_1", name: readPdf.name, input: {} }]),
              Message.tool({
                id: "call_pdf_1",
                name: readPdf.name,
                resultType: "content",
                result: [
                  { type: "text", text: "PDF read successfully" },
                  {
                    type: "file",
                    uri: `data:application/pdf;base64,${PDF}`,
                    mime: "application/pdf",
                    name: "verification",
                  },
                ],
              }),
            ],
            tools: [readPdf],
          }),
        ),
    },
  },
  {
    group: "wrapped-core",
    scenario: {
      cassette: "azure/chat-streams-text",
      protocol: "openai-chat",
      provider: {
        package: "@opencode/ai/providers/azure/chat",
        modelID: "gpt-5.6-luna",
        settings: { resourceName: azureResourceName, apiKey: "fixture" },
      },
      run: () => LLMClient.generate(ai.request({ model: azureChat, prompt: "Reply with exactly one word: hello" })),
    },
  },
  {
    group: "wrapped-core",
    scenario: {
      cassette: "azure/chat-streams-text-from-a-foundry-models-url",
      protocol: "openai-chat",
      provider: {
        package: "@opencode/ai/providers/azure/chat",
        modelID: "Kimi-K2.5",
        settings: { baseURL: azureFoundryBaseURL, apiKey: "fixture" },
      },
      run: () =>
        LLMClient.generate(ai.request({ model: azureFoundryChat, prompt: "Reply with exactly one word: hello" })),
    },
  },
  {
    group: "wrapped-core",
    scenario: {
      cassette: "azure/messages-streams-text",
      protocol: "anthropic-messages",
      provider: {
        package: "@opencode/ai/providers/azure/messages",
        modelID: "claude-haiku-4-5",
        settings: { resourceName: azureResourceName, apiKey: "fixture" },
      },
      run: () => LLMClient.generate(ai.request({ model: azureMessages, prompt: "Reply with exactly one word: hola" })),
    },
  },
  {
    group: "wrapped-core",
    scenario: {
      cassette: "azure/messages-calls-a-tool",
      protocol: "anthropic-messages",
      provider: {
        package: "@opencode/ai/providers/azure/messages",
        modelID: "claude-haiku-4-5",
        settings: { resourceName: azureResourceName, apiKey: "fixture" },
      },
      run: () =>
        LLMClient.generate(
          ai.request({
            model: azureMessages,
            prompt: "What is the weather in Paris? Use the lookup_weather tool.",
            tools: [azureLookupWeather],
          }),
        ),
    },
  },
  {
    group: "wrapped-core",
    scenario: {
      cassette: "azure/responses-streams-text",
      protocol: "openai-responses",
      provider: {
        package: "@opencode/ai/providers/azure/responses",
        modelID: "gpt-5.6-luna",
        settings: { resourceName: azureResourceName, apiKey: "fixture" },
      },
      run: () =>
        LLMClient.generate(ai.request({ model: azureResponses, prompt: "Reply with exactly one word: bonjour" })),
    },
  },
  {
    group: "wrapped-core",
    scenario: {
      cassette: "azure/responses-calls-a-tool",
      protocol: "openai-responses",
      provider: {
        package: "@opencode/ai/providers/azure/responses",
        modelID: "gpt-5.6-luna",
        settings: { resourceName: azureResourceName, apiKey: "fixture" },
      },
      run: () =>
        LLMClient.generate(
          ai.request({
            model: azureResponses,
            prompt: "What is the weather in Paris? Use the lookup_weather tool.",
            tools: [azureLookupWeather],
          }),
        ),
    },
  },
  {
    group: "wrapped-core",
    scenario: {
      cassette: "azure/responses-continues-after-a-tool-result",
      protocol: "openai-responses",
      provider: {
        package: "@opencode/ai/providers/azure/responses",
        modelID: "gpt-5.6-luna",
        settings: { resourceName: azureResourceName, apiKey: "fixture" },
      },
      run: () =>
        LLMClient.generate(
          ai.request({
            model: azureResponses,
            messages: [
              Message.user("What is the weather in Paris?"),
              Message.assistant([
                ToolCallPart.make({ id: "call_paris_1", name: "lookup_weather", input: { city: "Paris" } }),
              ]),
              Message.tool({
                id: "call_paris_1",
                name: "lookup_weather",
                result: "18C, light rain",
                resultType: "text",
              }),
            ],
            tools: [azureLookupWeather],
          }),
        ),
    },
  },
  {
    group: "wrapped-core",
    scenario: {
      cassette: "bedrock-mantle/streams-text",
      protocol: "open-responses",
      provider: {
        package: "@opencode/ai/providers/amazon-bedrock/mantle/responses",
        modelID: "openai.gpt-oss-120b",
        settings: mantleOptions,
      },
      run: () =>
        LLMClient.generate(
          ai.request({ model: mantleResponses, prompt: "Reply with exactly: hello", maxTokens: 256, temperature: 0 }),
        ),
    },
  },
  {
    group: "wrapped-core",
    scenario: {
      cassette: "bedrock-mantle-messages/replays-signed-thinking-through-a-tool-loop-and-native-system-update",
      protocol: "anthropic-messages",
      provider: {
        package: "@opencode/ai/providers/amazon-bedrock/mantle/messages",
        modelID: "anthropic.claude-opus-4-8",
        settings: mantleOptions,
      },
      redact: { allowRequestHeaders: ["anthropic-version", "anthropic-beta"] },
      run: () =>
        Effect.gen(function* () {
          const model = mantleMessages("anthropic.claude-opus-4-8")
          const initial = ai.request({
            model,
            system: "You are a concise assistant.",
            prompt:
              "First calculate 37 * 43. Then call get_weather for Paris. After receiving the tool result, state both the product and the weather.",
            tools: [mantleWeather],
            providerOptions: { thinking: { type: "adaptive", display: "summarized" }, effort: "medium" },
            maxTokens: 2048,
          })
          const first = yield* LLMClient.generate(initial)
          yield* LLMClient.generate({
            ...initial,
            messages: [
              ...initial.messages,
              first.message,
              ...first.toolCalls.map((call) =>
                Message.tool({ id: call.id, name: call.name, result: { condition: "sunny", temperatureC: 18 } }),
              ),
              Message.system("Reply in French in one short sentence."),
            ],
          })
        }),
    },
  },
  {
    group: "wrapped-core",
    scenario: {
      cassette: "bedrock-mantle-messages/lowers-system-updates-to-wrapped-user-text-on-haiku-4-5-with-budget-thinking",
      protocol: "anthropic-messages",
      provider: {
        package: "@opencode/ai/providers/amazon-bedrock/mantle/messages",
        modelID: "anthropic.claude-haiku-4-5",
        settings: mantleOptions,
      },
      redact: { allowRequestHeaders: ["anthropic-version", "anthropic-beta"] },
      run: () =>
        LLMClient.generate(
          ai.request({
            model: mantleMessages("anthropic.claude-haiku-4-5"),
            messages: [Message.user("What is 19 multiplied by 23?"), Message.system("Reply with only the integer.")],
            providerOptions: { thinking: { type: "enabled", budgetTokens: 1024 } },
            maxTokens: 2048,
          }),
        ),
    },
  },
  {
    group: "wrapped-core",
    scenario: {
      cassette:
        "bedrock-mantle-messages/applies-mid-conversation-effort-updates-and-thinking-block-binding-on-opus-5-5",
      protocol: "anthropic-messages",
      provider: {
        package: "@opencode/ai/providers/amazon-bedrock/mantle/messages",
        modelID: "anthropic.claude-opus-5-5",
        settings: mantleOptions,
      },
      redact: { allowRequestHeaders: ["anthropic-version", "anthropic-beta"] },
      run: () =>
        Effect.gen(function* () {
          const firstRequest = ai.request({
            model: mantleMessages("anthropic.claude-opus-5-5"),
            prompt: "Compute 37 * 43 step by step, then reply with only the integer.",
            providerOptions: { thinking: { type: "adaptive", display: "summarized" }, effort: "high" },
            maxTokens: 2048,
          })
          const first = yield* LLMClient.generate(firstRequest)
          yield* LLMClient.generate(
            ai.request({
              model: mantleMessages("anthropic.claude-opus-5-5"),
              messages: [
                ...firstRequest.messages,
                first.message,
                Message.effort({ effort: "low", previous: "high" }),
                Message.user("Add 9 to that result. Reply with only the integer."),
              ],
              providerOptions: { thinking: { type: "adaptive", display: "summarized" }, effort: "low" },
              maxTokens: 2048,
            }),
          )
        }),
    },
  },
  {
    group: "wrapped-core",
    scenario: {
      cassette: "bedrock-mantle-messages/strips-unsupported-mid-conversation-effort-updates-on-opus-5-0",
      protocol: "anthropic-messages",
      provider: {
        package: "@opencode/ai/providers/amazon-bedrock/mantle/messages",
        modelID: "anthropic.claude-opus-5",
        settings: mantleOptions,
      },
      redact: { allowRequestHeaders: ["anthropic-version", "anthropic-beta"] },
      run: () =>
        LLMClient.generate(
          ai.request({
            model: mantleMessages("anthropic.claude-opus-5"),
            messages: [
              Message.user("What is 12 + 30?"),
              Message.assistant("42"),
              Message.effort({ effort: "low", previous: "high" }),
              Message.user("Add 8 to that result. Reply with only the integer."),
            ],
            providerOptions: { thinking: { type: "adaptive", display: "summarized" }, effort: "low" },
            maxTokens: 1024,
          }),
        ),
    },
  },
  {
    group: "gateways",
    scenario: {
      cassette: "cloudflare-ai-gateway/cloudflare-ai-gateway-workers-ai-llama-3-1-8b-text",
      protocol: "openai-chat",
      provider: {
        package: "@opencode/ai/providers/cloudflare-ai-gateway",
        modelID: "workers-ai/@cf/meta/llama-3.1-8b-instruct",
        settings: { accountId: cloudflareAccountId, gatewayApiKey: "fixture" },
      },
      redact: cloudflareRedact,
      run: () =>
        runGoldenScenario("text", {
          id: "recorded_cloudflare_ai_gateway_workers_ai_llama_3_1_8b_text",
          model: cloudflareAIGatewayWorkers,
        }),
    },
  },
  {
    group: "gateways",
    scenario: {
      cassette: "cloudflare-ai-gateway/cloudflare-ai-gateway-workers-ai-gpt-oss-20b-tools-tool-call",
      protocol: "openai-chat",
      provider: {
        package: "@opencode/ai/providers/cloudflare-ai-gateway",
        modelID: "workers-ai/@cf/openai/gpt-oss-20b",
        settings: { accountId: cloudflareAccountId, gatewayApiKey: "fixture" },
      },
      redact: cloudflareRedact,
      run: () =>
        runGoldenScenario("tool-call", {
          id: "recorded_cloudflare_ai_gateway_workers_ai_gpt_oss_20b_tools_tool_call",
          model: cloudflareAIGatewayWorkersTools,
          maxTokens: 120,
        }),
    },
  },
  {
    group: "gateways",
    scenario: {
      cassette: "cloudflare-ai-gateway/cloudflare-ai-gateway-claude-haiku-4-5-text",
      protocol: "anthropic-messages",
      provider: {
        package: "@opencode/ai/providers/cloudflare-ai-gateway",
        modelID: "anthropic/claude-haiku-4.5",
        settings: { accountId: cloudflareAccountId, gatewayApiKey: "fixture" },
      },
      redact: cloudflareClaudeRedact,
      run: () =>
        runGoldenScenario("text", {
          id: "recorded_cloudflare_ai_gateway_claude_haiku_4_5_text",
          model: cloudflareAIGatewayClaude,
        }),
    },
  },
  {
    group: "gateways",
    scenario: {
      cassette: "cloudflare-ai-gateway/cloudflare-ai-gateway-claude-haiku-4-5-tool-loop",
      protocol: "anthropic-messages",
      provider: {
        package: "@opencode/ai/providers/cloudflare-ai-gateway",
        modelID: "anthropic/claude-haiku-4.5",
        settings: { accountId: cloudflareAccountId, gatewayApiKey: "fixture" },
      },
      redact: cloudflareClaudeRedact,
      run: () =>
        runGoldenScenario("tool-loop", {
          id: "recorded_cloudflare_ai_gateway_claude_haiku_4_5_tool_loop",
          model: cloudflareAIGatewayClaude,
        }),
    },
  },
  {
    group: "gateways",
    scenario: {
      cassette: "cloudflare-ai-gateway/cloudflare-ai-gateway-openai-gpt-5-nano-text",
      protocol: "openai-responses",
      provider: {
        package: "@opencode/ai/providers/cloudflare-ai-gateway",
        modelID: "openai/gpt-5-nano",
        settings: { accountId: cloudflareAccountId, gatewayApiKey: "fixture" },
      },
      redact: cloudflareRedact,
      run: () =>
        runGoldenScenario("text", {
          id: "recorded_cloudflare_ai_gateway_openai_gpt_5_nano_text",
          model: cloudflareAIGatewayOpenAI,
          temperature: false,
          maxTokens: 2000,
        }),
    },
  },
  {
    group: "gateways",
    scenario: {
      cassette: "cloudflare-ai-gateway/cloudflare-ai-gateway-openai-gpt-5-nano-tool-loop",
      protocol: "openai-responses",
      provider: {
        package: "@opencode/ai/providers/cloudflare-ai-gateway",
        modelID: "openai/gpt-5-nano",
        settings: { accountId: cloudflareAccountId, gatewayApiKey: "fixture" },
      },
      redact: cloudflareRedact,
      run: () =>
        runGoldenScenario("tool-loop", {
          id: "recorded_cloudflare_ai_gateway_openai_gpt_5_nano_tool_loop",
          model: cloudflareAIGatewayOpenAI,
          temperature: false,
          maxTokens: 2000,
        }),
    },
  },
  {
    group: "gateways",
    scenario: {
      cassette: "cloudflare-workers-ai/cloudflare-workers-ai-llama-3-1-8b-text",
      protocol: "openai-chat",
      provider: {
        package: "@opencode/ai/providers/cloudflare-workers-ai",
        modelID: "@cf/meta/llama-3.1-8b-instruct",
        settings: { accountId: cloudflareAccountId, apiKey: "fixture" },
      },
      redact: cloudflareRedact,
      run: () =>
        runGoldenScenario("text", {
          id: "recorded_cloudflare_workers_ai_llama_3_1_8b_text",
          model: cloudflareWorkersAI,
        }),
    },
  },
  {
    group: "gateways",
    scenario: {
      cassette: "cloudflare-workers-ai/cloudflare-workers-ai-gpt-oss-20b-tools-tool-call",
      protocol: "openai-chat",
      provider: {
        package: "@opencode/ai/providers/cloudflare-workers-ai",
        modelID: "@cf/openai/gpt-oss-20b",
        settings: { accountId: cloudflareAccountId, apiKey: "fixture" },
      },
      redact: cloudflareRedact,
      run: () =>
        runGoldenScenario("tool-call", {
          id: "recorded_cloudflare_workers_ai_gpt_oss_20b_tools_tool_call",
          model: cloudflareWorkersAITools,
          maxTokens: 120,
        }),
    },
  },
  {
    group: "gateways",
    scenario: {
      cassette: "vercel-native/continues-messages-claude-reasoning-and-tools",
      protocol: "anthropic-messages",
      provider: {
        package: "@opencode/ai/providers/vercel-ai-gateway",
        modelID: "anthropic/claude-sonnet-5.5",
        settings: vercelGatewaySettings,
      },
      run: () =>
        runWeatherToolLoop({
          ...goldenWeatherToolLoopRequest({
            id: "messages Claude",
            model: vercelGateway.messages("anthropic/claude-sonnet-5.5"),
            maxTokens: 2048,
            temperature: false,
          }),
          providerOptions: { reasoningEffort: "low" },
        }),
    },
  },
  {
    group: "gateways",
    scenario: {
      cassette: "vercel-native/continues-messages-gemini-reasoning-and-tools",
      protocol: "anthropic-messages",
      provider: {
        package: "@opencode/ai/providers/vercel-ai-gateway",
        modelID: "google/gemini-3.8-flash",
        settings: vercelGatewaySettings,
      },
      run: () =>
        runWeatherToolLoop({
          ...goldenWeatherToolLoopRequest({
            id: "messages Gemini",
            model: vercelGateway.messages("google/gemini-3.8-flash"),
            maxTokens: 2048,
            temperature: false,
          }),
          providerOptions: { reasoningEffort: "low" },
        }),
    },
  },
  {
    group: "gateways",
    scenario: {
      cassette: "vercel-native/continues-responses-gpt-reasoning-and-tools",
      protocol: "open-responses",
      provider: {
        package: "@opencode/ai/providers/vercel-ai-gateway",
        modelID: "openai/gpt-6-luna",
        settings: vercelGatewaySettings,
      },
      run: () =>
        runWeatherToolLoop({
          ...goldenWeatherToolLoopRequest({
            id: "responses GPT",
            model: vercelGateway.model("openai/gpt-6-luna"),
            maxTokens: 2048,
            temperature: false,
          }),
          providerOptions: { reasoningEffort: "low" },
        }),
    },
  },
  {
    group: "gateways",
    scenario: {
      cassette: "vercel-native/continues-chat-muse-reasoning-and-tools",
      protocol: "openai-chat",
      provider: {
        package: "@opencode/ai/providers/vercel-ai-gateway",
        modelID: "meta/muse-spark-1.3",
        settings: vercelGatewaySettings,
      },
      run: () =>
        runWeatherToolLoop({
          ...goldenWeatherToolLoopRequest({
            id: "chat Muse",
            model: vercelGateway.model("meta/muse-spark-1.3"),
            maxTokens: 2048,
            temperature: false,
          }),
          providerOptions: { reasoningEffort: "low" },
        }),
    },
  },
  {
    group: "gateways",
    scenario: {
      cassette: "vercel-native/continues-messages-deepseek-reasoning-and-tools",
      protocol: "anthropic-messages",
      provider: {
        package: "@opencode/ai/providers/vercel-ai-gateway",
        modelID: "deepseek/deepseek-v4.1-flash",
        settings: vercelGatewaySettings,
      },
      run: () =>
        runWeatherToolLoop({
          ...goldenWeatherToolLoopRequest({
            id: "messages DeepSeek",
            model: vercelGateway.model("deepseek/deepseek-v4.1-flash"),
            maxTokens: 2048,
            temperature: false,
          }),
          providerOptions: { reasoningEffort: "high" },
        }),
    },
  },
  {
    group: "gateways",
    scenario: {
      cassette: "vercel-native/continues-responses-grok-reasoning-and-tools",
      protocol: "open-responses",
      provider: {
        package: "@opencode/ai/providers/vercel-ai-gateway",
        modelID: "spacexai/grok-4.7",
        settings: vercelGatewaySettings,
      },
      run: () =>
        runWeatherToolLoop({
          ...goldenWeatherToolLoopRequest({
            id: "responses Grok",
            model: vercelGateway.model("spacexai/grok-4.7"),
            maxTokens: 2048,
            temperature: false,
          }),
          providerOptions: { reasoningEffort: "low" },
        }),
    },
  },
  {
    group: "gateways",
    scenario: {
      cassette: "vercel-native/continues-chat-claude-reasoning-and-tools",
      protocol: "openai-chat",
      provider: {
        package: "@opencode/ai/providers/vercel-ai-gateway/chat",
        modelID: "anthropic/claude-sonnet-5.5",
        settings: vercelGatewaySettings,
      },
      run: () =>
        runWeatherToolLoop({
          ...goldenWeatherToolLoopRequest({
            id: "chat Claude",
            model: vercelGatewayChat.chat("anthropic/claude-sonnet-5.5"),
            maxTokens: 2048,
            temperature: false,
          }),
          providerOptions: { reasoningEffort: "low" },
        }),
    },
  },
  {
    group: "gateways",
    scenario: {
      cassette: "vercel-native/rejects-invalid-credentials-without-retryable-classification",
      protocol: "anthropic-messages",
      provider: {
        package: "@opencode/ai/providers/vercel-ai-gateway",
        modelID: "anthropic/claude-sonnet-5.5",
        settings: { apiKey: "invalid-gateway-key" },
      },
      run: () =>
        LLMClient.generate(
          ai.request({
            model: VercelAIGateway.configure({ apiKey: "invalid-gateway-key" }).messages("anthropic/claude-sonnet-5.5"),
            prompt: "Hello",
            maxTokens: 32,
          }),
        ).pipe(Effect.flip),
    },
  },
  {
    group: "gateways",
    scenario: {
      cassette: "vercel-native/automatically-caches-distinct-claude-system-blocks",
      protocol: "anthropic-messages",
      provider: {
        package: "@opencode/ai/providers/vercel-ai-gateway",
        modelID: "anthropic/claude-sonnet-5.5",
        settings: vercelGatewaySettings,
      },
      run: () =>
        Effect.gen(function* () {
          const request = ai.request({
            model: vercelGateway.messages("anthropic/claude-sonnet-5.5"),
            system: [
              {
                type: "text",
                text: Array.from(
                  { length: 250 },
                  (_, i) => `Rule ${i}: Preserve provider reasoning state and keep stable prompt prefixes intact.`,
                ).join("\n"),
              },
              { type: "text", text: "Reply with OK only." },
            ],
            prompt: "Confirm.",
            maxTokens: 32,
            providerOptions: { gateway: { only: ["anthropic"] } },
          })
          yield* LLMClient.generate(request)
          yield* LLMClient.generate(request)
        }),
    },
  },
  ...chatVariantScenarios,
  ...vendorVariantScenarios,
  ...nativeVendorScenarios,
  ...embeddingScenarios,
]

const cassettes = []
const groups: Record<string, string> = {}
for (const item of scenarios) {
  console.error(`scenarios: ${item.scenario.cassette}`)
  const cassette = await capture(tree, item.scenario)
  cassettes.push(cassette)
  groups[cassette.cassette] = item.group
}
const fixtures = await multipartFixtures(load)
process.stdout.write(JSON.stringify({ cassettes, groups, multipartFixtures: fixtures }))
