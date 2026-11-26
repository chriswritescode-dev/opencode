import { AISDKNative } from "@opencode/core/aisdk-native"
import { Model } from "@opencode/core/model"
import { decodeSnapshot } from "@opencode/core/models-dev"
import { Config } from "@opencode/core/config"
import { ConfigPolicyPlugin } from "@opencode/core/config/plugin/policy"
import { ConfigProviderPlugin } from "@opencode/core/config/plugin/provider"
import { AppNodeBuilder } from "@opencode/core/effect/app-node-builder"
import { Integration } from "@opencode/core/integration"
import { ModelsDev } from "@opencode/core/models-dev"
import { Plugin } from "@opencode/core/plugin"
import { PluginHost } from "@opencode/core/plugin/host"
import { ModelsDevPlugin } from "@opencode/core/plugin/models-dev"
import { AmazonBedrockPlugin } from "@opencode/core/plugin/provider/amazon-bedrock"
import { AzurePlugin } from "@opencode/core/plugin/provider/azure"
import { CerebrasPlugin } from "@opencode/core/plugin/provider/cerebras"
import { ChatGPTPlugin } from "@opencode/core/plugin/provider/chatgpt"
import { CloudflareAIGatewayPlugin } from "@opencode/core/plugin/provider/cloudflare-ai-gateway"
import { CloudflareWorkersAIPlugin } from "@opencode/core/plugin/provider/cloudflare-workers-ai"
import { DatabricksPlugin } from "@opencode/core/plugin/provider/databricks"
import { DigitalOceanPlugin } from "@opencode/core/plugin/provider/digitalocean"
import { GithubCopilotPlugin } from "@opencode/core/plugin/provider/github-copilot"
import { GitLabPlugin } from "@opencode/core/plugin/provider/gitlab"
import { GoogleVertexPlugin } from "@opencode/core/plugin/provider/google-vertex"
import { KiloPlugin } from "@opencode/core/plugin/provider/kilo"
import { LLMGatewayPlugin } from "@opencode/core/plugin/provider/llmgateway"
import { LMStudioPlugin } from "@opencode/core/plugin/provider/lmstudio"
import { ModalPlugin } from "@opencode/core/plugin/provider/modal"
import { NeonPlugin } from "@opencode/core/plugin/provider/neon"
import { NvidiaPlugin } from "@opencode/core/plugin/provider/nvidia"
import { OllamaPlugin } from "@opencode/core/plugin/provider/ollama"
import { OpenAIPlugin } from "@opencode/core/plugin/provider/openai"
import { OpenAIContextCapsPlugin } from "@opencode/core/plugin/provider/openai-context-caps"
import { OpenRouterPlugin } from "@opencode/core/plugin/provider/openrouter"
import { PoePlugin } from "@opencode/core/plugin/provider/poe"
import { PromptCacheKeyPlugin } from "@opencode/core/plugin/provider/prompt-cache-key"
import { SnowflakeCortexPlugin } from "@opencode/core/plugin/provider/snowflake-cortex"
import { VLLMPlugin } from "@opencode/core/plugin/provider/vllm"
import { XAIPlugin } from "@opencode/core/plugin/provider/xai"
import { ZenmuxPlugin } from "@opencode/core/plugin/provider/zenmux"
import { WellKnownPlugin } from "@opencode/core/wellknown/plugin"
import { Document, Info } from "@opencode/schema/config"
import { mkdtemp, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { makePluginTestLayer } from "./plugin/fixture"
import { OauthCallbackPage } from "@opencode/core/oauth/page"
import { OauthRedirect } from "@opencode/core/oauth/redirect"
import { Credential } from "@opencode/core/credential"
import { Provider } from "@opencode/core/provider"
import { Variant } from "@opencode/core/variant"
import { LayerNodePlatform } from "@opencode/util/effect/app-node-platform"
import { Context, Effect, Layer, Schema } from "effect"
import { HttpClient } from "effect/http"

const area = process.argv[2]

const CorpusLayer = makePluginTestLayer(undefined, undefined, [
  LayerNodePlatform.httpClient.replace(
    Layer.succeed(
      HttpClient.HttpClient,
      HttpClient.make((request) => Effect.fail(new Error(`corpus refuses ${request.method} ${request.url}`))),
    ),
  ),
])

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
    "pasted.empty-error-description": {
      call: "pasted",
      text: "http://127.0.0.1:1455/callback?state=state-1&error=access_denied&error_description=&code=code-1",
      state,
      result: pasted(
        "http://127.0.0.1:1455/callback?state=state-1&error=access_denied&error_description=&code=code-1",
      ),
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
  cases["rewrite.bedrock-credentials-fallback"] = rewriteCase(
    { specifier: "@ai-sdk/amazon-bedrock", providerID: "sample", modelID: "anthropic.claude-sonnet-4" },
    {
      settings: {
        endpoint: "https://${AWS_REGION}.amazonaws.com",
        region: "eu-west-1",
        credentials: { accessKeyId: "PARTIAL", sessionToken: "PARTIAL-SESSION" },
        accessKeyId: "AKIA",
        secretAccessKey: "TOPSECRET",
        sessionToken: "TOPSESSION",
      },
    },
  )
  cases["rewrite.bedrock-reasoning-strip"] = rewriteCase(
    { specifier: "@ai-sdk/amazon-bedrock", providerID: "sample" },
    {
      settings: {
        reasoningConfig: { type: 5, maxReasoningEffort: "high", budgetTokens: "9", extra: 1 },
        additionalModelRequestFields: { anthropic_beta: ["ok"], other: { keep: true } },
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
  cases["rewrite.invalid-fields"] = rewriteCase(
    { specifier: "@opencode/ai/providers/anthropic", providerID: "sample" },
    {
      settings: {
        apiKey: 5,
        baseURL: 5,
        headers: { "X-Ok": "1", "X-Bad": 2 },
        extraBody: [1],
        useCompletionUrls: "yes",
        auth: "basic",
        anthropicBeta: ["a", 2],
        serviceTier: 5,
        appName: 5,
        appUrl: 5,
        api_keys: { a: "k", b: 1 },
        reasoningConfig: { type: 5, display: "summarized", maxReasoningEffort: 7, budgetTokens: "9", extra: 1 },
        additionalModelRequestFields: { anthropic_beta: ["ok"], output_config: 5, reasoning: "no", other: { keep: true } },
        credentials: { accessKeyId: "AKIA", secretAccessKey: "S", sessionToken: 5, region: "r", extra: 1 },
        unknown: { keep: true },
      },
    },
  )
  cases["rewrite.invalid-variant-fields"] = rewriteCase(
    { specifier: "@ai-sdk/openai", providerID: "sample", modelID: "m" },
    {
      settings: { baseURL: 5, extraBody: { base: true } },
      variants: [
        { id: "a", settings: { baseURL: 5, apiKey: 7, extraBody: { keep: 1 }, headers: { "X-Bad": 2 } } },
        { id: "b", settings: { apiKey: "ok", headers: { "X-Ok": "1" } } },
      ],
    },
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

const sha256 = (value: string | Uint8Array) => new Bun.CryptoHasher("sha256").update(value).digest("hex")

function canonical(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map((item) => canonical(item)).join(",")}]`
  if (value !== null && typeof value === "object")
    return `{${Object.keys(value)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${canonical((value as Record<string, unknown>)[key])}`)
      .join(",")}}`
  return JSON.stringify(value)
}

function decoded(text: string, recommended: boolean) {
  return JSON.parse(JSON.stringify(Effect.runSync(decodeSnapshot(text, recommended))))
}

function decodeCase(input: unknown) {
  return {
    call: "decode",
    input,
    expected: { false: decoded(JSON.stringify(input), false), true: decoded(JSON.stringify(input), true) },
  }
}

const v1Catalog = {
  acme: {
    id: "acme",
    name: "Acme",
    env: ["ACME_API_KEY"],
    npm: "@ai-sdk/openai-compatible",
    api: "https://api.acme.test/v1",
    models: {
      "acme-1": {
        id: "acme-1",
        name: "Acme One",
        release_date: "2026-01-01",
        attachment: false,
        reasoning: true,
        reasoning_options: [
          { type: "effort", values: [null, "low", "high"] },
          { type: "budget_tokens", min: 1024 },
        ],
        temperature: true,
        tool_call: true,
        interleaved: { field: "vendor_reasoning" },
        cost: { input: 1, output: 2, tiers: [{ tier: { type: "context", size: 200000 }, input: 3, output: 4 }] },
        limit: { context: 128000, output: 8192 },
      },
    },
  },
}

async function modelsDev() {
  const cases: Record<string, unknown> = {}
  for (const name of ["models-dev", "models-dev-reasoning"])
    cases[`fixture.${name}`] = decodeCase(await Bun.file(`test/plugin/fixtures/${name}.json`).json())
  cases["v1.models"] = decodeCase(v1Catalog)
  const text = await Bun.file("src/models-dev/snapshot.txt").text()
  const left = decoded(text, false)
  const right = decoded(text, true)
  cases["snapshot.bytes"] = { call: "text", bytes: new TextEncoder().encode(text).length, sha256: sha256(text) }
  cases["snapshot.providers"] = {
    call: "snapshot",
    count: { false: left.length, true: right.length },
    providers: {
      false: Object.fromEntries(left.map((snapshot: { info: { id: string } }) => [snapshot.info.id, sha256(canonical(snapshot))])),
      true: Object.fromEntries(right.map((snapshot: { info: { id: string } }) => [snapshot.info.id, sha256(canonical(snapshot))])),
    },
  }
  return cases
}

async function catalog() {
  const output = console.log
  console.log = console.info = console.debug = console.warn = console.error = () => {}
  const temp = await mkdtemp(join(tmpdir(), "opencode-catalog-corpus-"))
  const decode = Schema.decodeUnknownSync(Info)
  const documents = (values: readonly unknown[]) =>
    values.map((value) => new Document({ type: "document", info: decode(value) }))
  const modelsLayer = (file: string) =>
    AppNodeBuilder.build(ModelsDev.node, [
      ModelsDev.node.replace(ModelsDev.configured({ file, fetch: false, snapshot: false })),
    ])
  const effects = {
    wellknown: WellKnownPlugin.Plugin,
    models_dev: ModelsDevPlugin,
    config_provider: ConfigProviderPlugin.Plugin,
    config_policy: ConfigPolicyPlugin.Plugin,
    amazon_bedrock: AmazonBedrockPlugin,
    azure: AzurePlugin,
    cerebras: CerebrasPlugin,
    chatgpt: ChatGPTPlugin,
    cloudflare_ai_gateway: CloudflareAIGatewayPlugin,
    cloudflare_workers_ai: CloudflareWorkersAIPlugin,
    databricks: DatabricksPlugin,
    digitalocean: DigitalOceanPlugin,
    github_copilot: GithubCopilotPlugin,
    gitlab: GitLabPlugin,
    google_vertex: GoogleVertexPlugin,
    kilo: KiloPlugin,
    llmgateway: LLMGatewayPlugin,
    lmstudio: LMStudioPlugin,
    modal: ModalPlugin,
    neon: NeonPlugin,
    nvidia: NvidiaPlugin,
    ollama: OllamaPlugin,
    openai: OpenAIPlugin,
    openai_context_caps: OpenAIContextCapsPlugin,
    openrouter: OpenRouterPlugin,
    poe: PoePlugin,
    prompt_cache_key: PromptCacheKeyPlugin,
    snowflake_cortex: SnowflakeCortexPlugin,
    vllm: VLLMPlugin,
    xai: XAIPlugin,
    zenmux: ZenmuxPlugin,
  }
  const plain = (value: unknown) => JSON.parse(JSON.stringify(value))
  const run = async (name: string, input: any) => {
    const entries = documents(input.entries ?? [])
    let models: string | undefined
    if (input.models !== undefined) {
      models = join(temp, `${name}.json`)
      await writeFile(models, JSON.stringify(input.models))
    }
    const environment = input.environment ?? {}
    const restore = Object.entries(environment).map(([key, value]) => {
      const previous = process.env[key]
      process.env[key] = value as string
      return () => {
        if (previous === undefined) delete process.env[key]
        else process.env[key] = previous
      }
    })
    try {
      const result = await Effect.runPromise(
        Effect.gen(function* () {
          const context = yield* Effect.context<any>()
          const host = yield* PluginHost.make(Context.get(context, Plugin.Service), "test")
          if (Array.isArray(input.credentials)) {
            const credentials = Context.get(context, Credential.Service)
            for (const entry of input.credentials)
              yield* credentials.create({
                id: Credential.ID.make(entry.id),
                integrationID: Integration.ID.make(entry.integrationID),
                value: entry.value,
                label: entry.label,
                activate: entry.activate,
              })
          }
          for (const name of input.plugins as (keyof typeof effects)[]) {
            const effect = effects[name]
              .effect(host)
              .pipe(
                Effect.provide(Config.testLayer(entries)),
                models === undefined ? (value: any) => value : Effect.provide(modelsLayer(models)),
              )
            yield* effect
          }
          const providers = Context.get(context, Provider.Service)
          const model = Context.get(context, Model.Service)
          const integrations = Context.get(context, Integration.Service)
          if (input.activate)
            yield* providers.transform((editor) => {
              for (const record of editor.list())
                editor.update(record.provider.id, (provider) => {
                  provider.activation = "enabled"
                })
            })
          const all = yield* providers.all()
          const small: Record<string, unknown> = {}
          for (const provider of all) small[provider.id] = (yield* model.small(provider.id)) ?? null
          return {
            providers: { all, available: yield* providers.available() },
            models: {
              all: yield* model.all(),
              available: yield* model.available(),
              typed: {
                chat: yield* model.available(["chat"]),
                decision: yield* model.available(["decision"]),
                image: yield* model.available(["image"]),
                video: yield* model.available(["video"]),
              },
              default: (yield* model.default()) ?? null,
              small,
            },
            integrations: yield* integrations.list(),
          }
        }).pipe(Effect.provide(CorpusLayer), Effect.scoped),
      )
      return plain(result)
    } finally {
      for (const undo of restore) undo()
    }
  }

  const api = (sdk: string, base = "https://api.acme.test/v1") => ({
    "generate-content": { base_url: base, sdk: { opencodeai: sdk } },
  })
  const sdkOnly = (name: string) => ({ "generate-content": { sdk: { opencodeai: name } } })
  const chat = (id: string, overview: Record<string, unknown> = {}) => ({
    id,
    type: "chat",
    name: id,
    release_date: "2026-01-01",
    modalities: { input: ["text"], output: ["text"] },
    capabilities: { tools: { supported: true }, reasoning: { supported: false } },
    limit: { context: 128_000, output: 32_000 },
    api: api("@opencode/ai/providers/openai-compatible"),
    ...overview,
  })
  const cases: Array<[string, any]> = [
    [
      "provider.key-auth",
      {
        plugins: ["config_provider"],
        entries: [{ providers: { litellm: { package: "aisdk:@ai-sdk/openai-compatible", models: { chat: {} } } } }],
      },
    ],
    [
      "provider.custom-defaults",
      {
        plugins: ["config_provider"],
        entries: [{ providers: { custom: { package: "aisdk:@ai-sdk/openai-compatible", models: { chat: {} } } } }],
      },
    ],
    [
      "provider.types",
      {
        plugins: ["config_provider"],
        entries: [
          {
            providers: {
              custom: {
                package: "aisdk:@ai-sdk/openai-compatible",
                models: { chat: {}, painter: { type: "image" } },
              },
            },
          },
        ],
      },
    ],
    [
      "provider.layered",
      {
        plugins: ["config_provider"],
        entries: [
          {
            model: "custom/first",
            providers: {
              custom: {
                name: "Configured",
                canonical: "anthropic",
                env: ["CUSTOM_API_KEY"],
                package: "native",
                headers: { first: "first", shared: "first" },
                models: {
                  chat: {
                    name: "First",
                    package: "aisdk:@ai-sdk/openai-compatible",
                    capabilities: { tools: true, input: ["text"], output: ["text"] },
                    limit: { context: 100, output: 50 },
                    cost: { input: 1, output: 2 },
                    settings: { retained: true },
                    headers: { first: "first", shared: "first" },
                    variants: [{ id: "fast", headers: { first: "first", shared: "first" } }],
                  },
                },
              },
            },
          },
          {
            model: "custom/default",
            providers: {
              custom: {
                package: "aisdk:@ai-sdk/openai-compatible",
                settings: { baseURL: "https://example.test" },
                headers: { last: "last", shared: "last" },
                models: {
                  default: { name: "Default" },
                  chat: {
                    modelID: "api-chat",
                    name: "Last",
                    limit: { output: 75 },
                    headers: { last: "last", shared: "last" },
                    variants: [
                      { id: "fast", headers: { last: "last", shared: "last" } },
                      { id: "slow", headers: { slow: "slow" } },
                    ],
                  },
                },
              },
            },
          },
          { providers: { custom: { name: "Renamed" } } },
        ],
      },
    ],
    [
      "provider.base-url",
      {
        plugins: ["config_provider"],
        entries: [
          {
            providers: {
              custom: {
                package: "aisdk:@ai-sdk/openai-compatible",
                settings: { baseURL: "https://provider.test" },
                models: {
                  kept: { settings: { baseURL: "https://model.test", retained: true } },
                  plain: {},
                },
              },
            },
          },
        ],
      },
    ],
    [
      "modelsdev.first",
      {
        plugins: ["models_dev"],
        activate: true,
        entries: [{ experimental: { recommended_api: false } }],
        models: await Bun.file("test/plugin/fixtures/models-dev.json").json(),
      },
    ],
    [
      "modelsdev.recommended",
      {
        plugins: ["models_dev"],
        activate: true,
        entries: [{ experimental: { recommended_api: true } }],
        models: await Bun.file("test/plugin/fixtures/models-dev.json").json(),
      },
    ],
    [
      "modelsdev.reasoning",
      {
        plugins: ["models_dev"],
        activate: true,
        entries: [{ experimental: { recommended_api: false } }],
        models: await Bun.file("test/plugin/fixtures/models-dev-reasoning.json").json(),
      },
    ],
    [
      "modelsdev.filtering",
      {
        plugins: ["models_dev"],
        activate: true,
        entries: [{ experimental: { recommended_api: false } }],
        models: {
          "azure-cognitive-services": {
            id: "azure-cognitive-services",
            name: "Azure Cognitive Services",
            env: [],
            models: { legacy: chat("legacy") },
          },
          "google-vertex-anthropic": {
            id: "google-vertex-anthropic",
            name: "Google Vertex Anthropic",
            env: [],
            models: { legacy: chat("legacy") },
          },
          acme: {
            id: "acme",
            name: "Acme",
            env: ["ACME_API_KEY"],
            models: {
              current: chat("current"),
              legacy: chat("legacy", { status: "deprecated" }),
              "video-gen": chat("video-gen", {
                type: "video",
                modalities: { input: ["text"], output: ["video"] },
                api: api("@opencode/ai/providers/google/videos"),
              }),
              image: chat("image", {
                type: "image",
                modalities: { input: ["text"], output: ["image"] },
                api: api("@opencode/ai/providers/google/images"),
              }),
              decision: chat("decision", {
                type: "decision",
                api: api("@opencode/ai/providers/openai-compatible/decisions"),
              }),
              transcribe: chat("transcribe", { modalities: { input: ["audio"], output: ["text"] } }),
            },
          },
          local: { id: "local", name: "Local", env: [], models: {} },
        },
      },
    ],
    [
      "modelsdev.bedrock",
      {
        plugins: ["models_dev"],
        activate: true,
        entries: [{ experimental: { recommended_api: false } }],
        models: {
          "amazon-bedrock": {
            id: "amazon-bedrock",
            name: "Amazon Bedrock",
            env: ["AWS_ACCESS_KEY_ID"],
            models: {
              "anthropic.claude-opus-5": chat("anthropic.claude-opus-5", {
                api: { "generate-content": { sdk: { aisdk: "@ai-sdk/amazon-bedrock" } } },
              }),
              "in-region": chat("in-region", {
                api: { "generate-content": { sdk: { aisdk: "@ai-sdk/amazon-bedrock" } } },
              }),
            },
          },
        },
      },
    ],
    [
      "policy.denied-provider",
      {
        plugins: ["config_provider", "config_policy"],
        entries: [
          {
            policies: [{ action: "provider.use", resource: "custom", effect: "deny" }],
            providers: { custom: { package: "aisdk:@ai-sdk/openai-compatible", models: { chat: {} } } },
          },
        ],
      },
    ],
    [
      "policy.denied-integration",
      {
        plugins: ["config_provider", "config_policy"],
        entries: [
          {
            policies: [{ action: "integration.use", resource: "custom", effect: "deny" }],
            providers: {
              custom: { package: "aisdk:@ai-sdk/openai-compatible", models: { chat: {} } },
              kept: { package: "aisdk:@ai-sdk/openai-compatible", models: { chat: {} } },
            },
          },
        ],
      },
    ],
    [
      "plugin.amazon_bedrock",
      {
        plugins: ["models_dev", "amazon_bedrock"],
        activate: true,
        entries: [{ experimental: { recommended_api: false } }],
        environment: { AWS_ACCESS_KEY_ID: "AKIAIOSFODNN7EXAMPLE" },
        models: {
          "amazon-bedrock": {
            id: "amazon-bedrock",
            name: "Amazon Bedrock",
            env: ["AWS_ACCESS_KEY_ID"],
            models: {
              "anthropic.claude-sonnet-4": chat("anthropic.claude-sonnet-4", {
                api: api("@opencode/ai/providers/amazon-bedrock"),
              }),
            },
          },
        },
      },
    ],
    [
      "plugin.azure",
      {
        plugins: ["models_dev", "azure"],
        activate: true,
        entries: [{ experimental: { recommended_api: false } }],
        environment: { AZURE_RESOURCE_NAME: "my-models", PATH: "/nonexistent" },
        models: {
          azure: {
            id: "azure",
            name: "Azure",
            env: ["AZURE_API_KEY"],
            models: {
              "gpt-5": chat("gpt-5", { api: sdkOnly("@opencode/ai/providers/azure/responses") }),
            },
          },
        },
      },
    ],
    [
      "plugin.cerebras",
      {
        plugins: ["models_dev", "cerebras"],
        activate: true,
        entries: [{ experimental: { recommended_api: false } }],
        models: {
          cerebras: {
            id: "cerebras",
            name: "Cerebras",
            env: ["CEREBRAS_API_KEY"],
            models: {
              "llama-3.3-70b": chat("llama-3.3-70b", {
                api: api("@opencode/ai/providers/cerebras", "https://api.cerebras.ai/v1"),
              }),
            },
          },
        },
      },
    ],
    [
      "plugin.chatgpt",
      {
        plugins: ["models_dev", "chatgpt"],
        activate: true,
        entries: [{ experimental: { recommended_api: false } }],
        models: {
          openai: {
            id: "openai",
            name: "OpenAI",
            env: ["OPENAI_API_KEY"],
            models: {
              "gpt-5": chat("gpt-5", { api: api("@opencode/ai/providers/openai", "https://api.openai.com/v1") }),
            },
          },
        },
      },
    ],
    [
      "plugin.cloudflare_ai_gateway",
      {
        plugins: ["models_dev", "cloudflare_ai_gateway"],
        activate: true,
        entries: [{ experimental: { recommended_api: false } }],
        environment: { CLOUDFLARE_ACCOUNT_ID: "account-1", CLOUDFLARE_GATEWAY_ID: "gateway-1" },
        models: {
          "cloudflare-ai-gateway": {
            id: "cloudflare-ai-gateway",
            name: "Cloudflare AI Gateway",
            env: ["CLOUDFLARE_API_TOKEN"],
            models: {
              "gpt-5": chat("gpt-5", { api: sdkOnly("@opencode/ai/providers/cloudflare-ai-gateway") }),
            },
          },
        },
      },
    ],
    [
      "plugin.cloudflare_workers_ai",
      {
        plugins: ["models_dev", "cloudflare_workers_ai"],
        activate: true,
        entries: [{ experimental: { recommended_api: false } }],
        environment: { CLOUDFLARE_ACCOUNT_ID: "account-1" },
        models: {
          "cloudflare-workers-ai": {
            id: "cloudflare-workers-ai",
            name: "Cloudflare Workers AI",
            env: ["CLOUDFLARE_API_KEY"],
            models: {
              "@cf/meta/llama": chat("@cf/meta/llama", {
                api: sdkOnly("@opencode/ai/providers/cloudflare-workers-ai"),
              }),
            },
          },
        },
      },
    ],
    [
      "plugin.databricks",
      {
        plugins: ["models_dev", "databricks"],
        activate: true,
        entries: [{ experimental: { recommended_api: false } }],
        environment: { DATABRICKS_HOST: "dbc-1234.cloud.databricks.com" },
        models: {
          databricks: {
            id: "databricks",
            name: "Databricks",
            env: ["DATABRICKS_TOKEN"],
            models: {
              "databricks-claude-sonnet-4": chat("databricks-claude-sonnet-4", {
                api: api(
                  "@opencode/ai/providers/openai-compatible",
                  "https://${DATABRICKS_HOST}/serving-endpoints",
                ),
              }),
            },
          },
        },
      },
    ],
    [
      "plugin.digitalocean",
      {
        plugins: ["models_dev", "digitalocean"],
        activate: true,
        entries: [{ experimental: { recommended_api: false } }],
        models: {
          digitalocean: {
            id: "digitalocean",
            name: "DigitalOcean",
            env: ["DIGITALOCEAN_ACCESS_TOKEN"],
            models: {
              "llama3.3-70b": chat("llama3.3-70b", {
                api: api("@opencode/ai/providers/openai-compatible", "https://api.digitalocean.com/v2/ai"),
              }),
            },
          },
        },
      },
    ],
    [
      "plugin.github_copilot",
      {
        plugins: ["models_dev", "github_copilot"],
        activate: true,
        entries: [{ experimental: { recommended_api: false } }],
        models: {
          "github-copilot": {
            id: "github-copilot",
            name: "GitHub Copilot",
            env: ["GITHUB_TOKEN"],
            models: {
              "gpt-5": chat("gpt-5", { api: sdkOnly("@opencode/ai/providers/openai") }),
              "gpt-5-chat-latest": chat("gpt-5-chat-latest", {
                api: sdkOnly("@opencode/ai/providers/openai"),
              }),
            },
          },
        },
      },
    ],
    [
      "plugin.gitlab",
      {
        plugins: ["models_dev", "gitlab"],
        activate: true,
        entries: [{ experimental: { recommended_api: false } }],
        models: {
          gitlab: {
            id: "gitlab",
            name: "GitLab",
            env: ["GITLAB_TOKEN"],
            models: {
              "duo-chat-opus": chat("duo-chat-opus", { api: sdkOnly("aisdk:gitlab-ai-provider") }),
            },
          },
        },
      },
    ],
    [
      "plugin.google_vertex",
      {
        plugins: ["models_dev", "google_vertex"],
        activate: true,
        entries: [{ experimental: { recommended_api: false } }],
        environment: { GOOGLE_VERTEX_PROJECT: "my-project", GOOGLE_VERTEX_LOCATION: "us-central1" },
        models: {
          "google-vertex": {
            id: "google-vertex",
            name: "Google Vertex",
            env: ["GOOGLE_VERTEX_API_KEY"],
            models: {
              "gemini-2.5-pro": chat("gemini-2.5-pro", {
                api: sdkOnly("@opencode/ai/providers/google-vertex"),
              }),
            },
          },
        },
      },
    ],
    [
      "plugin.kilo",
      {
        plugins: ["models_dev", "kilo"],
        activate: true,
        entries: [{ experimental: { recommended_api: false } }],
        models: {
          kilo: {
            id: "kilo",
            name: "Kilo",
            env: ["KILO_API_KEY"],
            models: {
              "kilo-chat": chat("kilo-chat", {
                api: api("@opencode/ai/providers/openai-compatible", "https://api.kilo.ai/api/gateway"),
              }),
            },
          },
        },
      },
    ],
    [
      "plugin.llmgateway",
      {
        plugins: ["models_dev", "llmgateway"],
        activate: true,
        entries: [{ experimental: { recommended_api: false } }],
        models: {
          llmgateway: {
            id: "llmgateway",
            name: "LLM Gateway",
            env: ["LLM_GATEWAY_API_KEY"],
            models: {
              "gpt-5": chat("gpt-5", {
                api: api("@opencode/ai/providers/openai-compatible", "https://api.llmgateway.io/v1"),
              }),
            },
          },
        },
      },
    ],
    [
      "plugin.lmstudio",
      {
        plugins: ["models_dev", "lmstudio"],
        activate: true,
        entries: [
          {
            experimental: { recommended_api: false },
            providers: { lmstudio: { settings: { baseURL: "http://127.0.0.1:1/v1" } } },
          },
        ],
        models: {
          lmstudio: {
            id: "lmstudio",
            name: "LM Studio",
            env: ["LMSTUDIO_API_KEY"],
            models: {
              "local-model": chat("local-model", {
                api: api("@opencode/ai/providers/openai-compatible", "http://127.0.0.1:1/v1"),
              }),
            },
          },
        },
      },
    ],
    [
      "plugin.modal",
      {
        plugins: ["models_dev", "modal"],
        activate: true,
        entries: [{ experimental: { recommended_api: false } }],
        models: {
          modal: {
            id: "modal",
            name: "Modal",
            env: ["MODAL_PROXY_TOKEN"],
            models: {
              "modal-model": chat("modal-model", {
                api: sdkOnly("@opencode/ai/providers/openai-compatible"),
              }),
            },
          },
        },
      },
    ],
    [
      "plugin.neon",
      {
        plugins: ["models_dev", "neon"],
        activate: true,
        entries: [{ experimental: { recommended_api: false } }],
        models: {
          neon: {
            id: "neon",
            name: "Neon",
            env: ["NEON_API_KEY"],
            models: {
              "neon-model": chat("neon-model", {
                api: api("@opencode/ai/providers/openai-compatible", "https://br.neon.tech/v1"),
              }),
            },
          },
        },
      },
    ],
    [
      "plugin.nvidia",
      {
        plugins: ["models_dev", "nvidia"],
        activate: true,
        entries: [{ experimental: { recommended_api: false } }],
        models: {
          nvidia: {
            id: "nvidia",
            name: "NVIDIA",
            env: ["NVIDIA_API_KEY"],
            models: {
              "deepseek-ai/deepseek-v4-pro": chat("deepseek-ai/deepseek-v4-pro", {
                api: api("@opencode/ai/providers/openai-compatible", "https://integrate.api.nvidia.com/v1"),
              }),
            },
          },
        },
      },
    ],
    [
      "plugin.ollama",
      {
        plugins: ["models_dev", "ollama"],
        activate: true,
        entries: [
          {
            experimental: { recommended_api: false },
            providers: { ollama: { settings: { baseURL: "http://127.0.0.1:1/v1" } } },
          },
        ],
        models: {
          ollama: {
            id: "ollama",
            name: "Ollama",
            env: ["OLLAMA_API_KEY"],
            models: {
              "llama3.2": chat("llama3.2", {
                api: api("@opencode/ai/providers/openai-compatible", "http://127.0.0.1:1/v1"),
              }),
            },
          },
        },
      },
    ],
    [
      "plugin.openai",
      {
        plugins: ["models_dev", "openai"],
        activate: true,
        entries: [{ experimental: { recommended_api: false } }],
        models: {
          openai: {
            id: "openai",
            name: "OpenAI",
            env: ["OPENAI_API_KEY"],
            models: {
              "gpt-5": chat("gpt-5", { api: api("@opencode/ai/providers/openai", "https://api.openai.com/v1") }),
            },
          },
        },
      },
    ],
    [
      "plugin.openai_context_caps",
      {
        plugins: ["models_dev", "openai_context_caps"],
        activate: true,
        entries: [{ experimental: { recommended_api: false } }],
        credentials: [
          {
            id: "cred-openai-context-caps",
            integrationID: "openai",
            label: "default",
            value: {
              type: "oauth",
              methodID: "chatgpt-browser",
              refresh: "refresh-1",
              access: "access-1",
              expires: 4102444800000,
            },
            activate: true,
          },
        ],
        models: {
          openai: {
            id: "openai",
            name: "OpenAI",
            env: ["OPENAI_API_KEY"],
            models: {
              "gpt-5": chat("gpt-5", {
                api: api("@opencode/ai/providers/openai", "https://api.openai.com/v1"),
                limit: { context: 600000, input: 600000, output: 128000 },
              }),
              "gpt-6.1": chat("gpt-6.1", {
                api: api("@opencode/ai/providers/openai", "https://api.openai.com/v1"),
                limit: { context: 600000, input: 600000, output: 128000 },
              }),
            },
          },
        },
      },
    ],
    [
      "plugin.openrouter",
      {
        plugins: ["models_dev", "openrouter"],
        activate: true,
        entries: [{ experimental: { recommended_api: false } }],
        models: {
          openrouter: {
            id: "openrouter",
            name: "OpenRouter",
            env: ["OPENROUTER_API_KEY"],
            models: {
              "gpt-5-chat-latest": chat("gpt-5-chat-latest", {
                api: api("@opencode/ai/providers/openrouter", "https://openrouter.ai/api/v1"),
              }),
              "normal-model": chat("normal-model", {
                api: api("@opencode/ai/providers/openrouter", "https://openrouter.ai/api/v1"),
              }),
            },
          },
        },
      },
    ],
    [
      "plugin.poe",
      {
        plugins: ["models_dev", "poe"],
        activate: true,
        entries: [{ experimental: { recommended_api: false } }],
        models: {
          poe: {
            id: "poe",
            name: "Poe",
            env: ["POE_API_KEY"],
            models: {
              "poe-chat": chat("poe-chat", {
                api: api("@opencode/ai/providers/openai-compatible", "https://api.poe.com/v1"),
              }),
            },
          },
        },
      },
    ],
    [
      "plugin.prompt_cache_key",
      {
        plugins: ["models_dev", "prompt_cache_key"],
        activate: true,
        entries: [{ experimental: { recommended_api: false } }],
        models: {
          venice: {
            id: "venice",
            name: "Venice",
            env: ["VENICE_API_KEY"],
            models: {
              "venice-uncensored": chat("venice-uncensored", {
                api: api("@opencode/ai/providers/openai-compatible", "https://api.venice.ai/api/v1"),
              }),
            },
          },
        },
      },
    ],
    [
      "plugin.snowflake_cortex",
      {
        plugins: ["models_dev", "snowflake_cortex"],
        activate: true,
        entries: [{ experimental: { recommended_api: false } }],
        environment: { SNOWFLAKE_ACCOUNT: "myorg-myaccount" },
        models: {
          "snowflake-cortex": {
            id: "snowflake-cortex",
            name: "Snowflake Cortex",
            env: ["SNOWFLAKE_CORTEX_TOKEN"],
            models: {
              "claude-3-5-sonnet": chat("claude-3-5-sonnet", {
                api: api(
                  "@opencode/ai/providers/openai-compatible",
                  "https://${SNOWFLAKE_ACCOUNT}.snowflakecomputing.com/api/v2/cortex/v1",
                ),
              }),
              "llama3.1-70b": chat("llama3.1-70b", {
                api: api(
                  "@opencode/ai/providers/openai-compatible",
                  "https://${SNOWFLAKE_ACCOUNT}.snowflakecomputing.com/api/v2/cortex/v1",
                ),
              }),
            },
          },
        },
      },
    ],
    [
      "plugin.vllm",
      {
        plugins: ["models_dev", "vllm"],
        activate: true,
        entries: [
          {
            experimental: { recommended_api: false },
            providers: { vllm: { settings: { baseURL: "http://127.0.0.1:1/v1" } } },
          },
        ],
        models: {
          vllm: {
            id: "vllm",
            name: "vLLM",
            env: ["VLLM_API_KEY"],
            models: {
              "vllm-model": chat("vllm-model", {
                api: api("@opencode/ai/providers/openai-compatible", "http://127.0.0.1:1/v1"),
              }),
            },
          },
        },
      },
    ],
    [
      "plugin.xai",
      {
        plugins: ["models_dev", "xai"],
        activate: true,
        entries: [{ experimental: { recommended_api: false } }],
        models: {
          xai: {
            id: "xai",
            name: "xAI",
            env: ["XAI_API_KEY"],
            models: {
              "grok-4": chat("grok-4", { api: api("@opencode/ai/providers/xai", "https://api.x.ai/v1") }),
            },
          },
        },
      },
    ],
    [
      "plugin.zenmux",
      {
        plugins: ["models_dev", "zenmux"],
        activate: true,
        entries: [{ experimental: { recommended_api: false } }],
        models: {
          zenmux: {
            id: "zenmux",
            name: "ZenMux",
            env: ["ZENMUX_API_KEY"],
            models: {
              "zenmux-model": chat("zenmux-model", {
                api: api("@opencode/ai/providers/openai-compatible", "https://zenmux.ai/api/v1"),
              }),
            },
          },
        },
      },
    ],
  ]

  const casesOutput: Record<string, unknown> = {}
  for (const [name, input] of cases) casesOutput[name] = { input, expected: await run(name, input) }
  console.log = output
  return casesOutput
}

const builders: Record<string, () => Record<string, unknown> | Promise<Record<string, unknown>>> = {
  oauth,
  "aisdk-native": aisdkNative,
  variants,
  "models-dev": modelsDev,
  catalog,
}

const build = area === undefined ? undefined : builders[area]

if (build === undefined) {
  console.error(`corpus: unknown area ${area ?? ""}`)
  process.exit(1)
}

process.stdout.write(`${JSON.stringify(await build(), null, 2)}\n`)
