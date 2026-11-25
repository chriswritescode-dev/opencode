# OR-46: the models.dev fetch timeout applies per attempt and body-read failures are retried

TypeScript: the shared HTTP client is wrapped in
`HttpClient.retryTransient({ retryOn: "errors-and-responses", times: 2, schedule: Schedule.exponential(200).pipe(Schedule.jittered) })`,
so transient transport errors and 408, 429, 500, 502, 503 or 504 responses are retried twice with a jittered
exponential delay starting at 200 ms (`packages/core/src/models-dev.ts:568-574` at the pinned commit
`73a0eae88de47f10068a0f9dc7a743638fa3d830`). `fetchPath` pipes `http.execute`, the 304 and status-ok handling and
`ok.text` through `Effect.timeout("10 seconds")` (`:604-617`), so that single timeout bounds the whole retried
execution including the body read; a request that reaches the budget is interrupted and is not retried again. A
body-read failure surfaces after `http.execute` and is outside the retry policy, so it is not retried.

Rust: `FETCH_TIMEOUT` is 10 seconds, `RETRIES` is 2 and `RETRY_BASE_MILLIS` is 200
(`crates/catalog/src/models_dev/service.rs:20-22`). `fetch_path` builds a new `reqwest` request per attempt with
`.timeout(FETCH_TIMEOUT)` (`:292-310`, timeout at `:297`), retries at most `RETRIES` times while `retryable` accepts
the result, waiting `200 ms * 2^attempt` multiplied by a random 0.8-1.2 factor (`:302-308`), and `retryable` accepts
any `reqwest` error and the same transient status set (`:384-390`). `send` maps both a failed request and a failed
`response.text()` to `ModelsDevError::Request` (`:371-382`), so a truncated body is retried too. Each attempt can
therefore consume the full 10 seconds and the total can exceed the TypeScript budget by the extra attempts and
waits.

Reason: `reqwest` exposes a timeout per request rather than a timeout over a whole retried operation, and a failed
body read is treated as a transient transport failure worth another attempt. Every parity scenario runs with
`OPENCODE_DISABLE_MODELS_FETCH=1` and a local `OPENCODE_MODELS_PATH` (`parity/lib/sandbox.ts:57-58`), and
`catalog-fixture` resolves its provider and model routes from its own fixture instead of downloading a catalog, so
no parity scenario performs a fetch.

Accepted: pending owner
