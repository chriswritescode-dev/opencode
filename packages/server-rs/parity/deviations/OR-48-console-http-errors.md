# OR-48: Console request failures carry Rust HTTP and parser text instead of Effect errors

TypeScript: every Console request goes through Effect's HTTP client, so a failure is an `HttpClientError` or a schema
`ParseError`. A non-2xx answer from `follow` is reported by `HttpClientResponse.filterStatusOk`
(`packages/core/src/plugin/provider/opencode.ts:718`) as `StatusCode: non 2xx status code (<status> <method> <url>)`.
`redirectTarget` names the protocol with its trailing colon (`ftp:`, `:738`). The device sign-in posts and gets through
`send`/`get`/`post` (`:858-880`), so its failures keep the Effect text, and the only wrapping is
`Invalid device verification URL: <url error>` (`:131-136`) and `Invalid OpenCode server URL: <url error>` (`:787-797`).
A malformed skill, web search or artifact body fails with the Effect schema decoder message (`:770`, `:452`,
`opencode-artifacts.ts:59`, `:72`, `:112`, `:196`).

Rust: `crates/catalog/src/provider_plugin/opencode/follow.rs:22` reports `HTTP status <status>` for a non-2xx final
answer, `:71` names the protocol without the colon (`ftp`), and the redirect parse messages at `:67-69` are Rust-only.
`crates/catalog/src/provider_plugin/http.rs:32-38` renders transport failures as `HTTP transport error: <message>` and
timeouts as `HTTP request timed out`. Device sign-in adds `OpenCode request failed:`, `OpenCode device token request
failed:`, `OpenCode token request failed:`, `Invalid OpenCode response:` and `Invalid OpenCode device token response:`
prefixes (`sign_in.rs:210`, `:281`, `:349`, `:353`, `:361`). Skill, web search and artifact bodies report the `serde_json`
message (`content.rs:197`, `websearch.rs:70-71`, `artifacts.rs:241`). Usage limits keep the Rust transport text for a
transport failure, while `usage_limits.rs:130-136` renders the 2xx-mismatch text exactly as the TypeScript server does, so
the usage-limits scenario's 503 body matches.

Reason: the Rust HTTP, URL and JSON layers produce their own messages, and the plugin shares them with the rest of the
catalog rather than mapping every error into an Effect-shaped string. The usage-limits 503 body is the only Console error
text a parity scenario compares, and it was aligned deliberately. The ported tests assert the prefixes and messages that
matter to users and to the classification, not every inner string.

Accepted: pending owner
