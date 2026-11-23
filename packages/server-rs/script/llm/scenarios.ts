import "./capture.js"
import { Effect, Schema, Stream } from "effect"
import { capture, createLoader, type Scenario } from "./capture.js"

const tree = process.env.OPENCODE_UPSTREAM_TREE
if (!tree) throw new Error("scenarios: OPENCODE_UPSTREAM_TREE is not set")

const load = createLoader(tree)

const { ai, CacheHint, LLMEvent, Media, Message, ToolCallPart, ToolChoice, ToolDefinition, ToolRuntime } =
  await load("packages/ai/src/effect.ts")
const { LLMClient } = await load("packages/ai/src/route.ts")
const {
  Anthropic,
  AnthropicCompatible,
  AmazonBedrock,
  AmazonBedrockMantle,
  Azure,
  Cerebras,
  DeepInfra,
  Google,
  OpenAI,
  OpenAICompatible,
  TogetherAI,
  Venice,
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
]

const cassettes = []
const groups: Record<string, string> = {}
for (const item of scenarios) {
  console.error(`scenarios: ${item.scenario.cassette}`)
  const cassette = await capture(tree, item.scenario)
  cassettes.push(cassette)
  groups[cassette.cassette] = item.group
}
process.stdout.write(JSON.stringify({ cassettes, groups }))
