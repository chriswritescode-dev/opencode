import "./capture.js"
import { FIXED_EPOCH_MS } from "./capture.js"
import { Schema } from "effect"

const tree = process.env.OPENCODE_UPSTREAM_TREE
if (!tree) throw new Error("errors: OPENCODE_UPSTREAM_TREE is not set")

const { httpFailure } = await import(`${tree}/packages/ai/src/route/executor.ts`)
const { AIErrorReason } = await import(`${tree}/packages/ai/src/schema/index.ts`)

const url = "https://api.example.test/v1/messages"

type FailureOptions = {
  status?: number
  data?: unknown
  responseHeaders?: Record<string, string>
  responseBody?: string
}

type FailureInput = FailureOptions & { message: string; url?: string }

const failure = (message: string, options: FailureOptions = {}): FailureInput => ({
  ...(options.status === undefined ? {} : { url }),
  message,
  ...options,
})

const overflowMessages = [
  "tokens in request more than max tokens allowed",
  "Requested token count exceeds the model's maximum context length of 131072 tokens.",
  "Requested input length 600010 exceeds maximum input length 131071",
  "Input length (265330) exceeds model's maximum context length (262144).",
  "Input length 131393 exceeds the maximum allowed input length of 131040 tokens.",
  "The input (516368 tokens) is longer than the model's context length (262144 tokens).",
  "The input is longer than the model's context length trace_id: 39d8c3a5c6c91dbc8dd9055f0b37e084",
  "Prompt has 5,958,968 tokens, but the configured context size is 256,000 tokens",
  "Range of input length should be [1, 129024]",
  "Too many tokens",
  "Input validation error: `inputs` tokens + `max_new_tokens` must be <= 131073. Given: 600035 `inputs` tokens and 16 `max_new_tokens`",
  "Token limit exceeded",
  "Prompt exceeds max length",
  "The input is too long for the selected model.",
  "This text request is too large for the model's configured input limit.",
  "litellm.ContextWindowExceededError: litellm.BadRequestError: litellm._pre_call_checks: Context Window exceeded for given call. No models have context window large enough for this call.",
]

const rateLimitTextMessages = [
  "Throttling error: Too many tokens, please wait before trying again.",
  "Rate limit exceeded, please retry after 30 seconds.",
  "Too many requests. Please slow down.",
]

const plainTextRateLimitMessages = [
  "Request rate increased too quickly",
  "Rate limit exceeded, please try again later",
  "Too many requests, please slow down",
]

const jsonRateLimitBodies = [
  '{"type":"error","error":{"type":"too_many_requests"}}',
  '{"type":"error","error":{"code":"rate_limit_exceeded"}}',
  '{"code":"bad_request","error":{"code":"rate_limit_exceeded"}}',
  '{"type":"error","error":{"code":"unknown","type":"too_many_requests"}}',
]

const overloadedCodeBodies = ['{"code":"resource_exhausted"}', '{"code":"service_unavailable"}', '{"code":"slow_down"}']

const retryableServerMessages = [
  "The model is currently at capacity due to high demand. Please try again in a few minutes, or use a higher service tier for priority processing.",
  "Try again",
  "Please retry your request shortly.",
  "You can retry the request.",
  "Try your request again.",
  "The service is temporarily at capacity.",
  "The model is overloaded.",
  "Service unavailable",
  "Internal server error",
  "The server is busy.",
  "Provider returned error",
  "Provider returned an error",
  "ResourceExhausted",
  "Upstream connection failed",
  "Exceeded request buffer limit while retrying upstream",
]

const azureBody = {
  error: {
    message: "The response was filtered",
    type: null,
    param: "prompt",
    code: "content_filter",
    status: 400,
    innererror: { code: "ResponsibleAIPolicyViolation", content_filter_result: {} },
  },
}
const azureInnerBody = {
  error: { message: "The response was filtered", code: null, innererror: { code: "ResponsibleAIPolicyViolation" } },
}

const invalidPromptOpenAIBody = {
  error: {
    message:
      "Invalid prompt: your prompt was flagged as potentially violating our usage policy. Please try again with a different prompt.",
    type: "invalid_request_error",
    param: null,
    code: "invalid_prompt",
  },
}
const invalidPromptAnthropicBody = {
  type: "error",
  error: { type: "invalid_request_error", message: "Output blocked by content filtering policy" },
}

const openRouterChatBody = {
  id: "gen-abc123",
  object: "chat.completion.chunk",
  error: {
    code: 403,
    message: "Your input was flagged",
    metadata: { error_type: "content_policy_violation", reasons: ["violence"], flagged_input: "..." },
  },
  choices: [{ index: 0, delta: { content: "" }, finish_reason: "error" }],
}
const openRouterResponsesBody = {
  type: "response.failed",
  response: {
    id: "resp_abc123",
    status: "failed",
    error: { code: "image_content_policy_violation", message: "Your input was flagged" },
    error_type: "content_policy_violation",
  },
}
const openRouterMessagesBody = {
  type: "error",
  error: { type: "invalid_request_error", message: "Claude refused to respond", error_type: "refusal" },
}

const cyberMessage =
  "This content was flagged for possible cybersecurity risk. If this seems wrong, try rephrasing your request."
const cyberHttpBody = {
  error: { message: cyberMessage, type: "invalid_request_error", param: null, code: "cyber_policy" },
}
const cyberFrameBody = { type: "error", sequence_number: 1, code: "cyber_policy", message: cyberMessage, param: null }

const alibabaBody = {
  request_id: "1537ecc9-6873-93f7-9865-6fe4eb1d7851",
  code: "DataInspectionFailed",
  message: "Green net check failed for text (input): Input data may contain inappropriate content.",
}

const moderationMessage =
  "Your request was rejected by the safety system. If you believe this is an error, contact us at help.openai.com and include the request ID req_123."
const moderationOpenAIBody = {
  error: {
    message: moderationMessage,
    type: "image_generation_user_error",
    param: null,
    code: "moderation_blocked",
    moderation_details: { moderation_stage: "input", categories: ["other"] },
  },
}
const moderationVercelBody = {
  error: {
    message: moderationMessage,
    type: "AI_APICallError",
    param: {
      error: moderationMessage,
      statusCode: 400,
      name: "AI_APICallError",
      message: moderationMessage,
      isRetryable: false,
    },
  },
}

const zenPolicyBody = {
  error: { type: "server_error", message: "Upstream request failed: [content_filter] The response was filtered" },
}

const geminiTermsMessage = "Request rejected in accordance with our terms of use and policies."
const geminiTermsBody = { error: { code: 403, message: geminiTermsMessage, status: "PERMISSION_DENIED" } }
const zenTermsBody = {
  error: { type: "server_error", message: `Upstream request failed: [403] ${geminiTermsMessage}` },
}

const mantleMessage = [
  "SubmitRequestFailure: code=-32602, msg=219 validation errors for ResponsesRequest",
  "input.list[union[EasyInputMessageParam,ResponseComputerToolCallParam]].2.ResponseComputerToolCallParam.pending_safety_checks",
  "  Field required [type=missing, input_value={'content': [...], 'role': 'assistant', 'type': 'message'}, input_type=dict]",
].join("\n")
const mantleBody = { response: { status: "failed", error: { code: "invalid_prompt", message: mantleMessage } } }

const outsidePolicyBody = { error: { message: "Unknown parameter: content_policy_id" } }

const zenTyped = (type: string, message: string) => ({ type: "error", error: { type, message } })
const zenSubstituted = (message: string) => ({
  error: { type: "server_error", message: `Upstream request failed: ${message}` },
})
const zenAccountCases: ReadonlyArray<
  [number, { error: { message: string; code?: unknown; status?: unknown; details?: unknown } }]
> = [
  [429, zenTyped("GoUsageLimitError", "Go usage limit exceeded")],
  [429, zenTyped("BudgetLimitError", "A budget policy for this model has been reached.")],
  [
    429,
    {
      error: {
        code: 429,
        status: "RESOURCE_EXHAUSTED",
        message: "A budget policy for this model has been reached.",
        details: [
          {
            "@type": "type.googleapis.com/google.rpc.ErrorInfo",
            reason: "BudgetLimitError",
            domain: "opencode.ai",
            metadata: { coverage: "models", resetsAt: "2026-11-01T00:00:00.000Z" },
          },
        ],
      },
    },
  ],
  [429, zenTyped("FreeUsageLimitError", "Rate limit exceeded. Please try again later.")],
  [402, zenTyped("CreditLimitExceeded", "Credit limit exceeded.")],
  [402, zenSubstituted("Insufficient account funds")],
  [402, zenSubstituted("Account invoice is overdue")],
  [429, zenSubstituted("Account budget exceeded")],
]

const zai = (code: string, message: string) => ({ error: { code, message } })
const zaiPlanCases = [
  zai("1113", "Insufficient balance or no resource package. Please recharge."),
  zai("1308", "Usage limit reached for 5 hours. Your limit will reset at 2026-10-01 00:00:00"),
  zai(
    "1309",
    "Your GLM Coding Plan package has expired and is temporarily unavailable. You can resume using it after renewing the subscription on the official website.",
  ),
  zai("1310", "Weekly/Monthly Limit Exhausted. Your limit will reset at 2026-10-01 00:00:00"),
  zai("1311", "Your current subscription plan does not yet include access to glm-5"),
  zai("1314", "Your enterprise package has expired. Please contact your enterprise administrator."),
  {
    type: "error",
    error: {
      type: "rate_limit_error",
      code: "1309",
      message:
        "[1309][Your GLM Coding Plan package has expired and is temporarily unavailable. You can resume using it after renewing the subscription on the official website.][20260929132151e73af01340d54b58]",
    },
  },
]
const zaiOverflowCases = [
  { error: { code: "1261", message: "Prompt 超长" } },
  { error: { code: "1261", message: "Prompt too long" } },
  {
    type: "error",
    error: { type: "invalid_request_error", code: "1261", message: "[1261][Prompt too long][2026092913]" },
  },
]
const zaiSensitiveMessage =
  "System detected potentially unsafe or sensitive content in input or generation. Please avoid using prompts that may generate sensitive content. Thank you for your cooperation."
const zaiThrottleCases = [
  { error: { code: "1302", message: "Rate limit reached for requests" } },
  { error: { code: "1305", message: "The service may be temporarily overloaded, please try again later" } },
]

const bedrockThrottleBodies: ReadonlyArray<[string, string]> = [
  [
    "Too many tokens, please wait before trying again.",
    '{"message":"Too many tokens, please wait before trying again."}',
  ],
  [
    "Too many tokens per day, please wait before trying again.",
    '{"message":"Too many tokens per day, please wait before trying again."}',
  ],
]

const throttleBodies: ReadonlyArray<[string, string]> = [
  [
    "Tokens per minute limit exceeded - too many tokens processed.",
    '{"message":"Tokens per minute limit exceeded - too many tokens processed.","type":"too_many_tokens_error","param":"quota","code":"token_quota_exceeded"}',
  ],
  [
    "Token limit reached: too many tokens per minute for this model.",
    '{"message":"Token limit reached: too many tokens per minute for this model."}',
  ],
  [
    "User 10s request rate limit exhausted (3 of 3).",
    '{"error":{"message":"User 10s request rate limit exhausted (3 of 3).","type":"rate_limit_exceeded","code":null}}',
  ],
]

const throttlingInputs: ReadonlyArray<FailureOptions> = [
  { status: 429, data: { code: "ThrottlingException" } },
  { status: 429, responseBody: '{"__type":"com.amazonaws.bedrock#ThrottlingException"}' },
  { responseBody: '{"exception":{"type":"throttlingException"}}' },
  { status: 429, responseHeaders: { "x-amzn-errortype": "ThrottlingException:http://internal.amazon.com/coral/" } },
]

const openaiSubstitutedBody = {
  error: { type: "server_error", message: "Upstream request failed: Model is unavailable." },
}
const anthropicSubstitutedBody = {
  type: "error",
  error: { type: "api_error", message: "Upstream request failed: Model is unavailable." },
}

const nestedCodeBodies = [
  '{"code":"bad_request","error":{"code":"usage_not_included"}}',
  '{"code":"bad_request","error":{"code":"server_error"}}',
  '{"code":"bad_request","error":{"type":"invalid_request_error"}}',
]

const unrecognizedBodies = [
  '{"error":{"message":"no_kv_space"}}',
  '{"type":"error","error":{"code":123}}',
  "not-json",
  "network error",
]

const envelopeBodies = [
  '{"type":"error","error":{"type":"overloaded_error","message":"Try again"}}',
  '{"error":{"code":"insufficient_quota","message":"Request failed"}}',
  '{"type":"response.failed","response":{"error":{"code":"authentication_error","message":"Denied"}}}',
  '{"error":{"code":429,"status":"RESOURCE_EXHAUSTED","message":"Try again"}}',
  '{"exception":{"type":"throttlingException","details":{"message":"Try again"}}}',
]

const sdkData = { error: { code: "authentication_error" } }

const workersMessage =
  "AiError: Ai: The estimated number of input and maximum output tokens (900047) exceeded this model context window limit (32000). (17851589-f2e6-405d-b67c-5550e52ee1c1)"
const workersBody = {
  errors: [{ message: workersMessage, code: 5021 }],
  success: false,
  result: {},
  messages: [],
}

const novaMessage =
  "The model returned the following errors: Input Tokens Exceeded: Number of input tokens exceeds maximum length. Please update the input to try again."
const novaBody = {
  headers: {
    ":exception-type": { type: "string", value: "validationException" },
    ":content-type": { type: "string", value: "application/json" },
    ":message-type": { type: "string", value: "exception" },
  },
  body: JSON.stringify({ message: novaMessage }),
}

const cohereOverflowMessage =
  "too many tokens: size limit exceeded by 168512 tokens. Try using shorter or fewer inputs. The limit for this model is 132000 tokens."
const cohereOutputMessage =
  "too many tokens: max tokens must be less than or equal to 4096, the maximum output length for this model - received 1000000."

const googleAuthBody = JSON.stringify({
  error: {
    code: 400,
    message: "API key not valid. Please pass a valid API key.",
    status: "INVALID_ARGUMENT",
    details: [
      {
        "@type": "type.googleapis.com/google.rpc.ErrorInfo",
        reason: "API_KEY_INVALID",
        domain: "googleapis.com",
      },
      {
        "@type": "type.googleapis.com/google.rpc.LocalizedMessage",
        locale: "en-US",
        message: "API key not valid. Please pass a valid API key.",
      },
    ],
  },
})

const invalidRequestBody =
  '{"error":{"message":"model not found","type":"invalid_request_error","code":"model_not_found"}}'

const inputs: Array<FailureInput> = [
  ...overflowMessages.map((message) => failure(message)),
  failure("413 status code (no body)"),
  failure('{"error":{"type":"request_too_large","message":"Request exceeds the maximum size"}}', { status: 400 }),
  failure("request too large", { status: 413 }),
  failure("upstream request entity too large", { status: 502 }),
  ...rateLimitTextMessages.map((message) => failure(message)),
  ...plainTextRateLimitMessages.map((message) => failure(message)),
  ...jsonRateLimitBodies.map((message) => failure(message)),
  ...overloadedCodeBodies.map((message) => failure(message)),
  ...retryableServerMessages.map((message) => failure(message)),
  failure("Provider request failed", { responseBody: "Please try again later." }),
  failure("Invalid credentials, try again", { status: 401 }),
  failure("Quota exceeded, try again", { status: 429 }),
  failure("Rate limit exceeded, try again"),
  failure("Upstream request failed: validation failed", { status: 400 }),
  failure("Try again", { status: 200 }),
  failure("The response was filtered", { status: 400, responseBody: JSON.stringify(azureBody) }),
  failure("The response was filtered", { status: 400, responseBody: JSON.stringify(azureInnerBody) }),
  failure(invalidPromptOpenAIBody.error.message, {
    status: 400,
    responseBody: JSON.stringify(invalidPromptOpenAIBody),
  }),
  failure(invalidPromptAnthropicBody.error.message, {
    status: 400,
    responseBody: JSON.stringify(invalidPromptAnthropicBody),
  }),
  failure(openRouterChatBody.error.message, { status: 403, responseBody: JSON.stringify(openRouterChatBody) }),
  failure(openRouterResponsesBody.response.error.message, {
    responseBody: JSON.stringify(openRouterResponsesBody),
  }),
  failure(openRouterMessagesBody.error.message, { responseBody: JSON.stringify(openRouterMessagesBody) }),
  failure(cyberMessage, { status: 400, responseBody: JSON.stringify(cyberHttpBody) }),
  failure(`cyber_policy: ${cyberMessage}`, { responseBody: JSON.stringify(cyberFrameBody) }),
  failure(alibabaBody.message, { status: 400, responseBody: JSON.stringify(alibabaBody) }),
  failure(moderationMessage, { status: 400, responseBody: JSON.stringify(moderationOpenAIBody) }),
  failure(moderationMessage, { status: 400, responseBody: JSON.stringify(moderationVercelBody) }),
  failure(zenPolicyBody.error.message, { status: 400, responseBody: JSON.stringify(zenPolicyBody) }),
  failure(geminiTermsMessage, { status: 403, responseBody: JSON.stringify(geminiTermsBody) }),
  failure(zenTermsBody.error.message, { status: 403, responseBody: JSON.stringify(zenTermsBody) }),
  failure(mantleMessage, { status: 200, responseBody: JSON.stringify(mantleBody) }),
  failure("Provider request failed with HTTP 400", {
    status: 400,
    responseBody: JSON.stringify(outsidePolicyBody),
  }),
  ...[408, 409].map((status) => failure(`HTTP ${status}`, { status })),
  ...[400, 404, 418, 422, 451].map((status) => failure(`HTTP ${status}`, { status })),
  failure("Payment Required", { status: 402 }),
  ...zenAccountCases.map(([status, body]) =>
    failure(body.error.message, { status, responseBody: JSON.stringify(body) }),
  ),
  ...zaiPlanCases.map((body) => failure(body.error.message, { status: 429, responseBody: JSON.stringify(body) })),
  ...zaiOverflowCases.map((body) => failure(body.error.message, { status: 400, responseBody: JSON.stringify(body) })),
  failure(zaiSensitiveMessage, {
    status: 400,
    responseBody: JSON.stringify({ error: { code: "1301", message: zaiSensitiveMessage } }),
  }),
  ...zaiThrottleCases.map((body) => failure(body.error.message, { status: 429, responseBody: JSON.stringify(body) })),
  ...bedrockThrottleBodies.map(([message, body]) => failure(message, { status: 429, responseBody: body })),
  ...throttleBodies.map(([message, body]) =>
    failure(message, { status: 429, responseBody: body, responseHeaders: { "retry-after-ms": "4000" } }),
  ),
  ...throttlingInputs.flatMap((options) =>
    ["Token limit exceeded", "Too many tokens, please try again later"].map((message) => failure(message, options)),
  ),
  failure("Token limit exceeded", {
    status: 429,
    responseHeaders: {
      "x-amzn-errortype": "ThrottlingException:http://internal.amazon.com/coral/com.amazon.coral.availability/",
    },
  }),
  failure("prompt is too long", { status: 400, data: { code: "ValidationException" } }),
  failure(openaiSubstitutedBody.error.message, {
    status: 400,
    responseBody: JSON.stringify(openaiSubstitutedBody),
  }),
  failure(anthropicSubstitutedBody.error.message, {
    status: 400,
    responseBody: JSON.stringify(anthropicSubstitutedBody),
  }),
  failure(openaiSubstitutedBody.error.message, { responseBody: JSON.stringify(openaiSubstitutedBody) }),
  failure(openaiSubstitutedBody.error.message, {
    status: 200,
    responseBody: JSON.stringify(openaiSubstitutedBody),
  }),
  ...nestedCodeBodies.map((message) => failure(message)),
  ...unrecognizedBodies.map((message) => failure(message)),
  ...envelopeBodies.map((body) => failure("Request failed", { responseBody: body })),
  ...[sdkData, JSON.stringify(sdkData)].map((data) =>
    failure("Request failed", { status: 400, responseBody: '{"message":"Request failed"}', data }),
  ),
  failure(workersMessage, { status: 413, responseBody: JSON.stringify(workersBody) }),
  failure(novaMessage, { responseBody: JSON.stringify(novaBody) }),
  failure(cohereOverflowMessage, {
    status: 400,
    responseBody: JSON.stringify({ error_type: "TOO_MANY_TOKENS", message: cohereOverflowMessage }),
  }),
  failure(cohereOutputMessage, {
    status: 400,
    responseBody: JSON.stringify({ error_type: "TOO_MANY_TOKENS", message: cohereOutputMessage }),
  }),
  failure("API key not valid. Please pass a valid API key.", { status: 400, responseBody: googleAuthBody }),
  failure("Incorrect API key provided. You can obtain an API key from https://console.x.ai.", {
    status: 400,
    responseBody: '{"code":"invalid-argument","error":"Incorrect API key provided."}',
  }),
  failure("Request failed", {
    responseBody: '{"error":{"message":"This model\'s maximum context length is 40960 tokens"}}',
  }),
  failure("Request failed", { responseBody: '{"error":{"code":"insufficient_quota"}}' }),
  failure("Unknown Model, please check the model code.", {
    responseBody: '{"type":"response.failed","response":{"error":{"code":"model_not_found"}}}',
  }),
  failure("Your GLM Coding Plan package has expired and is temporarily unavailable.", {
    responseBody: '{"type":"response.failed","response":{"error":{"code":"permission_denied"}}}',
  }),
  failure("model not found", { status: 404, responseBody: invalidRequestBody }),
  failure("Internal server error", { status: 500, responseHeaders: { "x-should-retry": "true" } }),
  failure("Internal server error", { status: 500, responseHeaders: { "x-should-retry": "false" } }),
  failure("rate limit exceeded", { status: 429, responseHeaders: { "retry-after-ms": "2500" } }),
  failure("rate limit exceeded", { status: 429, responseHeaders: { "retry-after": "3" } }),
  failure("rate limit exceeded", { status: 429, responseHeaders: { "retry-after": "" } }),
  failure("rate limit exceeded", { status: 429, responseHeaders: { "retry-after": "0x10" } }),
  failure("rate limit exceeded", {
    status: 429,
    responseHeaders: { "retry-after": new Date(FIXED_EPOCH_MS + 5000).toUTCString() },
  }),
  failure("rate limit exceeded", {
    status: 429,
    responseHeaders: { "retry-after": "Friday, 06-Nov-26 08:49:37 GMT" },
  }),
  failure("rate limit exceeded", {
    status: 429,
    responseHeaders: { "retry-after": "Sunday, 06-Nov-94 08:49:37 GMT" },
  }),
  failure("User 10s rate\tlimit exhausted (3 of 3).", { status: 429 }),
  failure("rate limit exceeded", {
    status: 429,
    responseHeaders: {
      "x-ratelimit-limit-requests": "5000",
      "x-ratelimit-remaining-requests": "4999",
      "x-ratelimit-reset-requests": "1s",
    },
  }),
  failure("rate limit exceeded", {
    status: 429,
    responseHeaders: {
      "anthropic-ratelimit-requests-limit": "1000",
      "anthropic-ratelimit-requests-remaining": "999",
      "anthropic-ratelimit-requests-reset": "2023-11-14T22:13:20Z",
    },
  }),
]

const rows = inputs.map((input) => {
  const error = httpFailure(input)
  const reason = JSON.parse(JSON.stringify(Schema.encodeSync(AIErrorReason)(error.reason)))
  delete reason.cause
  return { input, reason, retryable: error.isRetryable }
})

const requiredTags = [
  "Authentication",
  "ContentPolicy",
  "InvalidRequest",
  "ProviderInternal",
  "QuotaExceeded",
  "RateLimit",
  "UnknownProvider",
]
const tags = new Set(rows.map((row) => row.reason._tag))
const missingTags = requiredTags.filter((tag) => !tags.has(tag))
const classifications = new Set(
  rows.filter((row) => row.reason._tag === "InvalidRequest").map((row) => row.reason.classification),
)
const missingClassifications = ["context-overflow", "payload-too-large"].filter(
  (classification) => !classifications.has(classification),
)
if (missingTags.length > 0 || missingClassifications.length > 0) {
  console.error(
    `errors: missing tags [${missingTags.join(", ")}], missing InvalidRequest classifications [${missingClassifications.join(", ")}]`,
  )
  process.exit(1)
}

console.error(`errors: ${rows.length} rows, tags [${[...tags].sort().join(", ")}]`)
process.stdout.write(JSON.stringify(rows))
