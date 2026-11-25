import { AISDKNative } from "@opencode/core/aisdk-native"
import { OauthCallbackPage } from "@opencode/core/oauth/page"
import { OauthRedirect } from "@opencode/core/oauth/redirect"
import { Provider } from "@opencode/core/provider"
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

const builders: Record<string, () => Record<string, unknown>> = { oauth, "aisdk-native": aisdkNative }

const build = area === undefined ? undefined : builders[area]

if (build === undefined) {
  console.error(`corpus: unknown area ${area ?? ""}`)
  process.exit(1)
}

process.stdout.write(`${JSON.stringify(build(), null, 2)}\n`)
