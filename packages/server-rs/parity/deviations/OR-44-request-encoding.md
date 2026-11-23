# OR-44: zstd and gzip request bodies are rejected instead of compressed

TypeScript: `packages/ai/src/protocols/shared.ts:468-501` has `jsonPost` compress the encoded body when `request.http.encoding` is `zstd` or `gzip`, set `content-encoding` to that value and send the compressed bytes. `compressBody` looks up `node:zlib` at call time and falls back to plain JSON when the runtime has no zlib or its zstd export, and it also falls back when compression throws (pinned commit `73a0eae88de47f10068a0f9dc7a743638fa3d830`).

Rust: `crates/ai/src/route/transport/http.rs` `json_post` accepts the same `RequestEncoding` and sends the text bytes for an absent encoding or `RequestEncoding::None`, but returns `AiErrorReason::UnsupportedOperation` with `operation: "http.encoding"` for `Zstd` and `Gzip` through `unsupported_encoding`. `crates/ai/src/route/compile.rs` forwards `request.http.encoding` into that call, so the whole request fails before any bytes are sent.

Reason: this package has no zlib dependency and no corpus cassette sets `http.encoding`; the replay check compares prepared request bodies only for the cases `script/llm/scenarios.ts` generates, so a compressed request is never observed there. A request that sets `http.encoding: "zstd"` or `"gzip"` fails with `UnsupportedOperation` where TypeScript sends the compressed body.

Accepted: pending owner
