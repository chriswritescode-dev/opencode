# OR-44: request strings cannot contain lone UTF-16 surrogates

TypeScript: `packages/ai/src/route/client.ts:547` runs `prepareRequest`, which passes the resolved request through `sanitizeSurrogates` before building the body. `packages/ai/src/utils/sanitize.ts:10` replaces every string that is not `isWellFormed()` with `toWellFormed()`, and it visits nested arrays, records and keys, so a lone surrogate inside message text, a tool name or a provider-option key reaches the provider as U+FFFD (pinned commit `e54fbacb3d3aebc85f0e5ada0a5c490cc9333a91`).

Rust: there is no sanitizer. `LlmRequest` and its message types derive `serde::Deserialize` (`crates/ai/src/schema/messages.rs:323-325`), and `serde_json` rejects an escaped unpaired surrogate such as `"\ud800"` because a Rust `String` is UTF-8 and cannot represent one. Paired surrogates, including every astral character such as emoji, decode and round-trip normally.

Reason: this is a representation limit of the Rust string type, documented for JSON bodies in `OR-38-lone-surrogates.md`; the request corpus omits unpaired surrogates for the same reason. Input that TypeScript would silently rewrite to U+FFFD is rejected here with a decode error, and correctly paired characters are unaffected.

Accepted: pending owner
