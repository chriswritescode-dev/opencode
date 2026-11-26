# OR-48: the Console watch socket ignores proxy settings

TypeScript: `Socket.makeWebSocket` is provided by `packages/core/src/effect/websocket-constructor.ts:58-77`, which
selects a proxy for the URL from `WSS_PROXY`/`WS_PROXY`, `HTTPS_PROXY`/`HTTP_PROXY` or `ALL_PROXY`, skips loopback and
`NO_PROXY` matches (`:22-46`), and passes an `HttpProxyAgent` or `HttpsProxyAgent` to the Node WebSocket. The watch
transport uses it for the event stream (`packages/core/src/plugin/provider/opencode-watches.ts:494`).

Rust: `crates/catalog/src/provider_plugin/opencode/watches/transport.rs:229` opens the stream with
`tokio_tungstenite::connect_async`, which connects directly and reads no proxy environment variable. The long-poll
fallback uses the shared HTTP sender, which may follow the environment's proxy, but the WebSocket itself never does.

Reason: the Rust WebSocket client has no proxy seam, while the TypeScript layer selects one explicitly. A member behind a
proxy that only allows proxied egress keeps the watches dormant with a backoff on the WebSocket attempts instead of
connecting. Add proxy selection to the watch transport if the Rust server needs to honour those variables for the
socket.

Accepted: pending owner
