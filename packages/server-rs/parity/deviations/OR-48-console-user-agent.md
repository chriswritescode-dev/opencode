# OR-48: some Console requests omit the OpenCode user agent

TypeScript: the plugin wraps its HTTP client once with `HttpClientRequest.setHeader("User-Agent",
App.useragent(ctx.app))` (`packages/core/src/plugin/provider/opencode.ts:198`), so device sign-in, config and list
fetches, skill downloads, hosted web search, watch long polls, acknowledgements, watch-tool calls and artifact requests
all carry it. (The WebSocket upgrade passes only the Console headers, `opencode-watches.ts:494`.)

Rust: `crates/catalog/src/provider_plugin/opencode/sign_in.rs:373`, `remote.rs:213-215` and `content.rs:186-187` add the
user agent explicitly, but `websearch.rs:56-57`, `artifacts.rs:254-255`, the watch headers built in `opencode/mod.rs:548-556`
and the watch HTTP calls in `watches/transport.rs:345-348` send only `accept` and the Console credentials. The WebSocket
upgrade matches TypeScript and sends no user agent.

Reason: each Rust caller builds its own headers and there is no per-plugin client middleware. The Console reads the user
agent to tell which OpenCode a member runs, so these requests look unidentified; the usage-limits client had no user agent
upstream either. No scenario compares request headers beyond `authorization` and the organization headers.

Accepted: pending owner
