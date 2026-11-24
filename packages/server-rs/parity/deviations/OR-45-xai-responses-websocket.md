# OR-45: xAI streams over HTTP only and has no chat route

TypeScript: `packages/ai/src/providers/xai.ts:38-55` defines `xai-responses` with `transport: OpenResponsesChannel.transport({...})` (`:45-53`), which enables a WebSocket channel when the caller requests one, with `rotateAfterMs` (`:48`) and a `continuation` rule that drops `previous_response_id` when `store === false` (`:52`). `:57-66` registers `xai-chat` on the OpenAI Chat protocol, whose `headers` function (`:64-65`) returns `{ "x-grok-conv-id": request.promptCacheKey }` when a prompt cache key is set. Both routes are exported at `:68`.

Rust: `crates/ai/src/providers/xai.rs:24-35` builds only `xai-responses`, with `channel: None` (`:32`) and `transport: RouteTransport::Http` (`:33`); `crates/ai/src/providers/mod.rs:254-256` registers no xAI chat entry and nothing writes an `x-grok-conv-id` header. When a WebSocket executor is requested, `crates/ai/src/route/compile.rs:65-74` prepares a channel only when the route has one or its transport is `RouteTransport::OpenAiResponsesChannel`; otherwise the compiled channel is `None`, and `crates/ai/src/route/client.rs:153-183` executes the plain HTTP request.

Reason: the port reused its hand-written xAI Responses adapter and did not port the Open Responses channel configuration or the separate chat route. A requested WebSocket therefore silently falls back to HTTP SSE for xAI, and `promptCacheKey` produces no `x-grok-conv-id` header because no chat route exists. The replay corpus holds three WebSocket cassettes, all under `openai-responses-websocket/`, and no xAI WebSocket or xAI chat cassette; the two `pdf/xai-*` cassettes replay the responses route over HTTP, so neither missing path is exercised.

Accepted: pending owner
