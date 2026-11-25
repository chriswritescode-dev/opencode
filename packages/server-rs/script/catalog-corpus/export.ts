import { AISDKNative } from "@opencode/core/aisdk-native"
import { Model } from "@opencode/core/model"
import { OauthCallbackPage } from "@opencode/core/oauth/page"
import { OauthRedirect } from "@opencode/core/oauth/redirect"
import { Provider } from "@opencode/core/provider"
import { Variant } from "@opencode/core/variant"
import { Effect } from "effect"

const area = process.argv[2]

function oauth() {
  const state = "state-1"
  const escapedProvider = `xAI</script><script>alert("provider")</script>`
  const escapedTokenPath = `/token</script><script>alert("path")</script>`

  function pasted(text: string) {
    const outcome = Effect.runSync(Effect.result(OauthRedirect.pasted(text, state)))
    if (outcome._tag === "Failure") return { kind: "failure" as const, message: outcome.failure.message }
    const value = outcome.success
    const params = Array.from(value.params.entries())
    return "error" in value
      ? { kind: "error" as const, error: value.error, params }
      : { kind: "code" as const, code: value.code, params }
  }

  return {
    "success.default": {
      call: "success",
      options: null,
      html: OauthCallbackPage.success(),
    },
    "success.provider": {
      call: "success",
      options: { provider: "xAI" },
      html: OauthCallbackPage.success({ provider: "xAI" }),
    },
    "error.default": {
      call: "error",
      detail: "access_denied: The user denied the request.",
      options: null,
      html: OauthCallbackPage.error("access_denied: The user denied the request."),
    },
    "error.special": {
      call: "error",
      detail: `<script>alert("x")</script> & more`,
      options: null,
      html: OauthCallbackPage.error(`<script>alert("x")</script> & more`),
    },
    bootstrap: {
      call: "bootstrap",
      tokenPath: "/callback",
      provider: "xAI",
      html: OauthCallbackPage.bootstrap({ tokenPath: "/callback", provider: "xAI" }),
    },
    "bootstrap.escaped": {
      call: "bootstrap",
      tokenPath: escapedTokenPath,
      provider: escapedProvider,
      html: OauthCallbackPage.bootstrap({ tokenPath: escapedTokenPath, provider: escapedProvider }),
    },
    "pasted.valid": {
      call: "pasted",
      text: "http://127.0.0.1:1455/callback?state=state-1&code=code-1",
      state,
      result: pasted("http://127.0.0.1:1455/callback?state=state-1&code=code-1"),
    },
    "pasted.state-mismatch": {
      call: "pasted",
      text: "http://127.0.0.1:1455/callback?state=other&code=code-1",
      state,
      result: pasted("http://127.0.0.1:1455/callback?state=other&code=code-1"),
    },
    "pasted.error-description": {
      call: "pasted",
      text: "http://127.0.0.1:1455/callback?state=state-1&error=access_denied&error_description=The+user+denied+the+request",
      state,
      result: pasted(
        "http://127.0.0.1:1455/callback?state=state-1&error=access_denied&error_description=The+user+denied+the+request",
      ),
    },
    "pasted.error": {
      call: "pasted",
      text: "http://127.0.0.1:1455/callback?state=state-1&error=access_denied",
      state,
      result: pasted("http://127.0.0.1:1455/callback?state=state-1&error=access_denied"),
    },
    "pasted.missing-code": {
      call: "pasted",
      text: "http://127.0.0.1:1455/callback?state=state-1",
      state,
      result: pasted("http://127.0.0.1:1455/callback?state=state-1"),
    },
    "pasted.non-url": {
      call: "pasted",
      text: "not a url",
      state,
      result: pasted("not a url"),
    },
  }
}

type Shape = "responses" | "completions"

type Overlays = {
  settings?: Provider.Settings
  headers?: Record<string, string>
  body?: Record<string, unknown>
}

type Target = Overlays & {
  package?: string
  variants?: (Overlays & { id: string })[]
}

type RewriteInput = {
  specifier: string | null
  providerID: string
  canonical?: string
  modelID?: string
  shape?: Shape
}

const PACKAGES = [
  "@ai-sdk/amazon-bedrock",
  "@ai-sdk/alibaba",
  "@ai-sdk/anthropic",
  "@ai-sdk/azure",
  "@ai-sdk/cerebras",
  "@ai-sdk/cohere",
  "@ai-sdk/deepinfra",
  "@ai-sdk/google",
  "@ai-sdk/google-vertex",
  "@ai-sdk/google-vertex/anthropic",
  "@ai-sdk/groq",
  "@ai-sdk/mistral",
  "@ai-sdk/openai",
  "@ai-sdk/openai-compatible",
  "@ai-sdk/togetherai",
  "@ai-sdk/xai",
  "@ai-sdk/gateway",
  "@openrouter/ai-sdk-provider",
  "ai-gateway-provider",
  "venice-ai-sdk-provider",
]

const protocols = ["@ai-sdk/openai-compatible", "@ai-sdk/anthropic", "@ai-sdk/openai"]

const HOSTS: Record<string, string[]> = {
  alibaba: protocols,
  "alibaba-cn": protocols,
  "alibaba-coding-plan": protocols,
  "alibaba-coding-plan-cn": protocols,
  "alibaba-token-plan": protocols,
  "alibaba-token-plan-cn": protocols,
  baseten: ["@ai-sdk/openai-compatible"],
  "cloudflare-ai-gateway": ["@ai-sdk/anthropic", "@ai-sdk/openai", "@ai-sdk/openai-compatible", "ai-gateway-provider"],
  cohere: ["@ai-sdk/openai-compatible"],
  "cloudflare-workers-ai": ["@ai-sdk/openai-compatible"],
  deepseek: ["@ai-sdk/openai-compatible"],
  digitalocean: ["@ai-sdk/openai-compatible"],
  "fireworks-ai": ["@ai-sdk/openai-compatible"],
  "google-vertex": ["@ai-sdk/openai-compatible", "@ai-sdk/mistral"],
  "kimi-for-coding": protocols,
  meta: protocols,
  minimax: protocols,
  "minimax-cn": protocols,
  "minimax-coding-plan": protocols,
  "minimax-cn-coding-plan": protocols,
  moonshotai: protocols,
  "moonshotai-cn": protocols,
  zai: ["@ai-sdk/openai-compatible"],
  "zai-coding-plan": protocols,
  zhipuai: ["@ai-sdk/openai-compatible"],
  "zhipuai-coding-plan": protocols,
}

function nativeCase(npm: string, providerID: string, modelID?: string, shape?: Shape, settings?: Provider.Settings) {
  const context = {
    providerID,
    ...(modelID === undefined ? {} : { modelID }),
    ...(shape === undefined ? {} : { shape }),
    ...(settings === undefined ? {} : { settings }),
  }
  return {
    call: "native" as const,
    npm,
    ...context,
    result: AISDKNative.native(npm, context) ?? null,
  }
}

function rewriteCase(input: RewriteInput, target: Target = {}) {
  const recorded = JSON.parse(JSON.stringify(target)) as Target
  AISDKNative.rewrite(recorded, {
    ...input,
    specifier: input.specifier ?? undefined,
    builtins: Provider.builtinPackages,
  })
  return {
    call: "rewrite" as const,
    target,
    input,
    result: JSON.parse(JSON.stringify(recorded)),
  }
}

function aisdkNative() {
  const cases: Record<string, unknown> = {}
  for (const npm of PACKAGES) {
    for (const shape of ["responses", "completions", undefined] as const) {
      cases[`native.package.${npm}.${shape ?? "default"}`] = nativeCase(npm, "sample", "sample-model", shape)
    }
  }
  for (const [providerID, packages] of Object.entries(HOSTS)) {
    for (const npm of packages) {
      cases[`native.host.${providerID}.${npm}`] = nativeCase(npm, providerID)
    }
  }
  cases["native.mantle.default"] = nativeCase("@ai-sdk/amazon-bedrock/mantle", "sample")
  cases["native.mantle.anthropic"] = nativeCase("@ai-sdk/amazon-bedrock/mantle", "sample", "anthropic.claude-sonnet-4")
  cases["native.mantle.gpt-oss"] = nativeCase("@ai-sdk/amazon-bedrock/mantle", "sample", "openai.gpt-oss-120b")
  cases["native.mantle.gpt-5"] = nativeCase("@ai-sdk/amazon-bedrock/mantle", "sample", "openai.gpt-5")
  cases["native.azure.completions"] = nativeCase("@ai-sdk/azure", "sample", undefined, "completions")
  cases["native.azure.use-completion-urls"] = nativeCase("@ai-sdk/azure", "sample", undefined, undefined, {
    useCompletionUrls: true,
  })
  cases["native.unknown"] = nativeCase("aisdk:unknown-package", "sample")

  cases["rewrite.openai"] = rewriteCase({ specifier: "@ai-sdk/openai", providerID: "sample", modelID: "gpt-5" }, {})
  cases["rewrite.prefixed"] = rewriteCase({ specifier: "aisdk:@ai-sdk/anthropic", providerID: "sample" }, {})
  cases["rewrite.overlays"] = rewriteCase(
    { specifier: "@ai-sdk/openai", providerID: "sample" },
    {
      settings: {
        headers: { Authorization: "base", "X-Extra": "1" },
        extraBody: { nested: { a: 1, b: 2 }, list: [1] },
      },
      headers: { AUTHORIZATION: "target-header" },
      body: { nested: { b: 30 }, extra: true },
    },
  )
  cases["rewrite.openai-compatible"] = rewriteCase(
    { specifier: "@ai-sdk/openai-compatible", providerID: "sample" },
    { settings: {} },
  )
  cases["rewrite.openai-compatible-canonical"] = rewriteCase(
    { specifier: "@ai-sdk/openai-compatible", providerID: "sample", canonical: "canonical-id" },
    { settings: {} },
  )
  cases["rewrite.openai-compatible-builtin"] = rewriteCase(
    { specifier: "@opencode/ai/providers/openai-compatible", providerID: "sample" },
    { settings: {} },
  )
  cases["rewrite.bedrock-converse"] = rewriteCase(
    { specifier: "@ai-sdk/amazon-bedrock", providerID: "sample", modelID: "anthropic.claude-sonnet-4" },
    {
      settings: {
        bearerToken: "tok",
        endpoint: "https://${AWS_REGION}.amazonaws.com",
        credentials: {
          accessKeyId: "AKIA",
          secretAccessKey: "SECRET",
          sessionToken: "SESSION",
          region: "eu-west-1",
        },
        region: "us-east-1",
        reasoningConfig: {
          type: "enabled",
          budgetTokens: 2048,
          maxReasoningEffort: "high",
          display: "summarized",
        },
        anthropicBeta: ["beta-1"],
        additionalModelRequestFields: { anthropic_beta: ["beta-0"], nested: { keep: true } },
        serviceTier: "priority",
        useCompletionUrls: true,
        headers: { "X-Base": "1" },
        extraBody: { "to-be": "replaced" },
      },
    },
  )
  cases["rewrite.bedrock-gpt-oss"] = rewriteCase(
    { specifier: "@ai-sdk/amazon-bedrock", providerID: "sample", modelID: "openai.gpt-oss-120b" },
    { settings: { reasoningConfig: { type: "adaptive", maxReasoningEffort: "medium", budgetTokens: 5 } } },
  )
  cases["rewrite.bedrock-gpt-5"] = rewriteCase(
    { specifier: "@ai-sdk/amazon-bedrock", providerID: "sample", modelID: "openai.gpt-5" },
    { settings: { reasoningConfig: { maxReasoningEffort: "low" } } },
  )
  cases["rewrite.bedrock-other"] = rewriteCase(
    { specifier: "@ai-sdk/amazon-bedrock", providerID: "sample", modelID: "meta.llama" },
    { settings: { reasoningConfig: { type: "enabled", budgetTokens: 99, maxReasoningEffort: "high" } } },
  )
  cases["rewrite.bedrock-adaptive"] = rewriteCase(
    { specifier: "@ai-sdk/amazon-bedrock", providerID: "sample", modelID: "anthropic.claude-sonnet-4" },
    {
      settings: {
        apiKey: "explicit",
        bearerToken: "ignored",
        accessKeyId: "AKIA",
        secretAccessKey: "SECRET",
        sessionToken: "SESSION",
        region: "us-west-2",
        reasoningConfig: { type: "adaptive", maxReasoningEffort: "high", display: "summarized" },
        additionalModelRequestFields: { output_config: { existing: "x" }, reasoning: { keep: 1 } },
      },
    },
  )
  cases["rewrite.mantle"] = rewriteCase(
    { specifier: "aisdk:@ai-sdk/amazon-bedrock/mantle", providerID: "sample", modelID: "anthropic.claude-sonnet-4" },
    { settings: { endpoint: "https://${AWS_REGION}.example", region: "us-east-2", extraBody: { keep: true } } },
  )
  cases["rewrite.mantle-legacy"] = rewriteCase(
    { specifier: "@opencode/ai/providers/amazon-bedrock-mantle", providerID: "sample", modelID: "openai.gpt-oss-20b" },
    {},
  )
  cases["rewrite.openrouter"] = rewriteCase(
    { specifier: "@openrouter/ai-sdk-provider", providerID: "sample" },
    {
      settings: {
        appName: "app",
        appUrl: "https://example.test",
        api_keys: { one: "k1", two: "k2" },
        compatibility: "strict",
        headers: { "X-Existing": "1" },
      },
    },
  )
  cases["rewrite.variants"] = rewriteCase(
    { specifier: "@ai-sdk/openai", providerID: "sample", modelID: "m" },
    {
      settings: { extraBody: { base: true } },
      variants: [
        {
          id: "a",
          settings: { headers: { "X-V": "1" }, extraBody: { base: false, v: 1 } },
          headers: { "X-Override": "2" },
          body: { v: 2 },
        },
        { id: "b", headers: { X: "1" } },
        { id: "c", body: { k: 1 } },
        { id: "d" },
      ],
    },
  )
  cases["rewrite.legacy-vertex"] = rewriteCase(
    { specifier: "@opencode/ai/providers/google-vertex-chat", providerID: "sample" },
    {},
  )
  cases["rewrite.migrated-package"] = rewriteCase(
    { specifier: "@opencode-ai/ai/providers/anthropic", providerID: "sample" },
    { settings: {} },
  )
  cases["rewrite.builtin-passthrough"] = rewriteCase(
    { specifier: "@opencode/ai/providers/anthropic", providerID: "sample" },
    { settings: { apiKey: "x" } },
  )
  cases["rewrite.unknown"] = rewriteCase(
    { specifier: "aisdk:unknown-package", providerID: "sample" },
    { settings: { keep: true }, package: "keep" },
  )
  cases["rewrite.azure-chat"] = rewriteCase(
    { specifier: "@ai-sdk/azure", providerID: "sample", shape: "completions" },
    {},
  )
  cases["rewrite.azure-native"] = rewriteCase(
    { specifier: "@opencode/ai/providers/azure/responses", providerID: "sample", shape: "completions" },
    {},
  )
  cases["rewrite.host"] = rewriteCase({ specifier: "@ai-sdk/openai", providerID: "moonshotai", modelID: "m" }, {})
  cases["rewrite.no-specifier"] = rewriteCase({ providerID: "sample", specifier: null }, { settings: { keep: true } })

  return cases
}

const VARIANT_PACKAGES = [
  "@opencode/ai/providers/openai",
  "@opencode/ai/providers/openai/responses",
  "@opencode/ai/providers/azure",
  "@opencode/ai/providers/azure/responses",
  "@opencode/ai/providers/amazon-bedrock/mantle",
  "@opencode/ai/providers/amazon-bedrock/mantle/responses",
  "@opencode/ai/providers/alibaba/responses",
  "@opencode/ai/providers/meta",
  "@opencode/ai/providers/meta/responses",
  "@opencode/ai/providers/minimax/responses",
  "@opencode/ai/providers/moonshot/responses",
  "@opencode/ai/providers/zai-coding-plan/responses",
  "@opencode/ai/providers/openai-compatible/responses",
  "@opencode/ai/providers/openai-compatible-responses",
  "@opencode/ai/providers/google-vertex/responses",
  "@opencode/ai/providers/xai",
  "@opencode/ai/providers/digitalocean/responses",
  "@opencode/ai/providers/openrouter/responses",
  "@opencode/ai/providers/venice/responses",
  "@opencode/ai/providers/openai-compatible",
  "@opencode/ai/providers/openai/chat",
  "@opencode/ai/providers/azure/chat",
  "@opencode/ai/providers/amazon-bedrock/mantle/chat",
  "@opencode/ai/providers/google-vertex/chat",
  "@opencode/ai/providers/alibaba",
  "@opencode/ai/providers/alibaba/chat",
  "@opencode/ai/providers/baseten",
  "@opencode/ai/providers/cerebras",
  "@opencode/ai/providers/cloudflare-workers-ai",
  "@opencode/ai/providers/cohere/chat",
  "@opencode/ai/providers/deepinfra",
  "@opencode/ai/providers/deepseek",
  "@opencode/ai/providers/digitalocean",
  "@opencode/ai/providers/fireworks",
  "@opencode/ai/providers/groq",
  "@opencode/ai/providers/meta/chat",
  "@opencode/ai/providers/minimax/chat",
  "@opencode/ai/providers/mistral",
  "@opencode/ai/providers/google-vertex/mistral",
  "@opencode/ai/providers/moonshot",
  "@opencode/ai/providers/moonshot/chat",
  "@opencode/ai/providers/togetherai",
  "@opencode/ai/providers/venice",
  "@opencode/ai/providers/zai",
  "@opencode/ai/providers/zai/chat",
  "@opencode/ai/providers/zai-coding-plan",
  "@opencode/ai/providers/zai-coding-plan/chat",
  "@opencode/ai/providers/anthropic",
  "@opencode/ai/providers/anthropic-compatible",
  "@opencode/ai/providers/amazon-bedrock/mantle/messages",
  "@opencode/ai/providers/azure/messages",
  "@opencode/ai/providers/google-vertex/messages",
  "@opencode/ai/providers/google-vertex-messages",
  "@opencode/ai/providers/alibaba/messages",
  "@opencode/ai/providers/meta/messages",
  "@opencode/ai/providers/minimax",
  "@opencode/ai/providers/minimax/messages",
  "@opencode/ai/providers/moonshot/messages",
  "@opencode/ai/providers/zai-coding-plan/messages",
  "@opencode/ai/providers/google",
  "@opencode/ai/providers/google-vertex",
  "@opencode/ai/providers/google-vertex/gemini",
  "@opencode/ai/providers/google/images",
  "@opencode/ai/providers/google-vertex/images",
  "@opencode/ai/providers/google/interactions",
  "@opencode/ai/providers/google-vertex/interactions",
  "@opencode/ai/providers/amazon-bedrock",
  "@opencode/ai/providers/cohere",
  "@opencode/ai/providers/openrouter",
  "aisdk:venice-ai-sdk-provider",
  "@opencode/ai/providers/cloudflare-ai-gateway",
  "@opencode/ai/providers/vercel-ai-gateway",
  "@opencode/ai/providers/vercel-ai-gateway/chat",
  "@opencode/ai/providers/vercel-ai-gateway/messages",
  "@opencode/ai/providers/vercel-ai-gateway/responses",
  "aisdk:@ai-sdk/gateway",
  "aisdk:@jerome-benoit/sap-ai-provider-v2",
  "aisdk:gitlab-ai-provider",
]

function variants() {
  const cases: Record<string, unknown> = {}

  function variantCase(
    name: string,
    packageName: string,
    modelID: string,
    output: number,
    supports: Variant.Support[],
    providerID = "sample",
  ) {
    const model = Model.Info.default(Provider.ID.make(providerID), Model.ID.make(modelID)) as Model.MutableInfo
    model.package = packageName
    model.modelID = Model.ID.make(modelID)
    model.limit = { ...model.limit, output }
    cases[name] = {
      call: "resolve",
      model: { id: modelID, modelID, providerID, package: packageName, output },
      supports,
      result: Variant.resolve(Model.Info.make(model), supports).map((item) => ({ ...item, id: String(item.id) })),
    }
  }

  const sets: [string, Variant.Support[]][] = [
    ["toggle", [{ type: "toggle" }]],
    ["effort", [{ type: "effort" }]],
    ["budget", [{ type: "budget_tokens" }]],
  ]

  for (const packageName of VARIANT_PACKAGES) {
    for (const [suffix, supports] of sets) {
      variantCase(`${packageName}.${suffix}`, packageName, "test-model", 32000, supports)
    }
    variantCase(`${packageName}.budget-large`, packageName, "test-model", 262144, [{ type: "budget_tokens" }])
    variantCase(`${packageName}.mixed`, packageName, "test-model", 64000, [
      { type: "toggle" },
      { type: "effort" },
      { type: "budget_tokens" },
    ])
  }

  variantCase(
    "anthropic.opus-4-5",
    "@opencode/ai/providers/anthropic",
    "claude-opus-4-5",
    32000,
    [{ type: "effort", values: ["low", "high"] }, { type: "budget_tokens", min: 1024 }],
  )
  variantCase("anthropic.sonnet-3-7-version-first", "@opencode/ai/providers/anthropic", "claude-3-7-sonnet", 32000, [
    { type: "effort" },
  ])
  variantCase("anthropic.opus-6", "@opencode/ai/providers/anthropic", "claude-opus-6", 32000, [{ type: "effort" }])
  variantCase("anthropic.haiku-budget-64k", "@opencode/ai/providers/anthropic", "claude-haiku-4-5", 64000, [
    { type: "budget_tokens", min: 1024, max: 64000 },
  ])
  variantCase("cloudflare-gateway.xai", "@opencode/ai/providers/cloudflare-ai-gateway", "xai/grok-4.6", 32000, [
    { type: "effort", values: ["low", "high"] },
  ])
  variantCase(
    "cloudflare-gateway.anthropic",
    "@opencode/ai/providers/cloudflare-ai-gateway",
    "anthropic/claude-sonnet-4-6",
    32000,
    [{ type: "effort", values: ["low", "high"] }],
  )
  variantCase("vercel.alibaba-budget", "@opencode/ai/providers/vercel-ai-gateway", "alibaba/qwen-3-32b", 38912, [
    { type: "toggle" },
    { type: "budget_tokens", min: 1, max: 38912 },
  ])
  variantCase(
    "nvidia.deepseek",
    "@opencode/ai/providers/openai-compatible",
    "deepseek-ai/deepseek-v4-pro",
    32000,
    [{ type: "effort", values: ["none", "high", "max"] }],
    "nvidia",
  )
  variantCase(
    "nvidia.kimi-mixed",
    "@opencode/ai/providers/openai-compatible",
    "moonshotai/kimi-k3",
    32000,
    [{ type: "toggle" }, { type: "effort", values: ["low", "high", "max"] }],
    "nvidia",
  )
  variantCase(
    "nvidia.glm-toggle",
    "@opencode/ai/providers/openai-compatible",
    "zai-org/GLM-5.2",
    32000,
    [{ type: "toggle" }],
    "nvidia",
  )
  variantCase(
    "nvidia.nemotron-budget",
    "@opencode/ai/providers/openai-compatible",
    "nvidia/nemotron-3-super",
    32000,
    [{ type: "budget_tokens", min: 1024 }],
    "nvidia",
  )
  variantCase(
    "nvidia.nemotron-budget-large",
    "@opencode/ai/providers/openai-compatible",
    "nvidia/nemotron-3-super",
    262144,
    [{ type: "budget_tokens" }],
    "nvidia",
  )
  variantCase(
    "nvidia.nemotron-mixed",
    "@opencode/ai/providers/openai-compatible",
    "nvidia/nemotron-3-super",
    64000,
    [{ type: "toggle" }, { type: "effort" }, { type: "budget_tokens" }],
    "nvidia",
  )
  variantCase("zai.glm-5-3-effort", "@opencode/ai/providers/zai", "glm-5.3", 32000, [
    { type: "toggle" },
    { type: "effort", values: ["low", "high", "max"] },
  ])
  variantCase("zai.glm-4-7-toggle", "@opencode/ai/providers/zai", "glm-4.7", 32000, [{ type: "toggle" }])
  variantCase("alibaba.budget-cap", "@opencode/ai/providers/alibaba/chat", "kimi-k2.6", 262144, [
    { type: "toggle" },
    { type: "budget_tokens" },
  ])
  variantCase("alibaba.chat-effort", "@opencode/ai/providers/alibaba", "qwen3.8-max", 32000, [
    { type: "toggle" },
    { type: "effort", values: ["low", "medium", "xhigh"] },
    { type: "budget_tokens", min: 0, max: 262144 },
  ])
  variantCase(
    "bedrock.claude-budget",
    "@opencode/ai/providers/amazon-bedrock",
    "us.anthropic.claude-haiku-4-5-20251001-v1:0",
    64000,
    [{ type: "budget_tokens", min: 1024 }],
  )
  variantCase("bedrock.grok-effort", "@opencode/ai/providers/amazon-bedrock", "us.xai.grok-4.6", 32000, [
    { type: "effort", values: ["low", "xhigh"] },
  ])
  variantCase("bedrock.nova-effort", "@opencode/ai/providers/amazon-bedrock", "us.amazon.nova-2-lite-v1:0", 32000, [
    { type: "effort", values: ["low", "xhigh"] },
  ])
  variantCase(
    "google.interactions-toggle",
    "@opencode/ai/providers/google/interactions",
    "gemma-4-31b-it",
    32000,
    [{ type: "toggle" }],
  )
  variantCase("minimax.messages-toggle", "@opencode/ai/providers/minimax/messages", "MiniMax-M3", 32000, [
    { type: "toggle" },
  ])
  variantCase("moonshot.k3-messages", "@opencode/ai/providers/moonshot/messages", "k3", 32000, [
    { type: "toggle" },
    { type: "effort", values: ["low", "high", "max"] },
  ])
  variantCase("moonshot.k2-6-chat", "@opencode/ai/providers/moonshot/chat", "kimi-k2.6", 32000, [{ type: "toggle" }])
  variantCase(
    "gitlab.opus-effort",
    "aisdk:gitlab-ai-provider",
    "duo-chat-opus-5-5",
    32000,
    [{ type: "effort", values: ["low", "medium", "high", "xhigh", "max"] }],
    "gitlab",
  )
  variantCase(
    "sap.anthropic-effort",
    "aisdk:@jerome-benoit/sap-ai-provider-v2",
    "anthropic.claude-sonnet-4-6",
    32000,
    [{ type: "effort", values: ["low", "high"] }],
  )
  variantCase("sap.gemini-toggle", "aisdk:@jerome-benoit/sap-ai-provider-v2", "gemini-2.5-pro", 32000, [
    { type: "toggle" },
  ])
  variantCase(
    "workers-ai.effort",
    "@opencode/ai/providers/cloudflare-workers-ai",
    "@cf/deepseek-ai/deepseek-v4-flash-0731",
    32000,
    [{ type: "effort", values: ["none", "low", "high", "max"] }],
  )
  variantCase("cohere.budget", "@opencode/ai/providers/cohere", "command-a-reasoning-08-2025", 32000, [
    { type: "toggle" },
    { type: "budget_tokens", min: 1 },
  ])
  variantCase("baseten.glm-effort", "@opencode/ai/providers/baseten", "zai-org/GLM-5.2", 32000, [
    { type: "effort", values: ["none", "high", "max"] },
  ])
  variantCase("deepinfra.glm-effort", "@opencode/ai/providers/deepinfra", "zai-org/GLM-5.2", 32000, [
    { type: "toggle" },
    { type: "effort", values: ["low", "high", "xhigh"] },
  ])
  variantCase("openrouter.budget", "@opencode/ai/providers/openrouter", "test-model", 32000, [
    { type: "toggle" },
    { type: "budget_tokens" },
  ])
  variantCase("venice.toggle", "@opencode/ai/providers/venice", "test-model", 32000, [{ type: "toggle" }])
  variantCase("xai.responses-effort", "@opencode/ai/providers/xai", "grok-4.6", 32000, [
    { type: "effort", values: ["low", "medium", "high", "xhigh"] },
  ])
  variantCase("zai-coding-plan.messages-forced", "@opencode/ai/providers/zai-coding-plan/messages", "glm-5.3", 32000, [
    { type: "toggle" },
    { type: "effort", values: ["low", "high", "max"] },
  ])

  return cases
}

const builders: Record<string, () => Record<string, unknown>> = { oauth, "aisdk-native": aisdkNative, variants }

const build = area === undefined ? undefined : builders[area]

if (build === undefined) {
  console.error(`corpus: unknown area ${area ?? ""}`)
  process.exit(1)
}

process.stdout.write(`${JSON.stringify(build(), null, 2)}\n`)
