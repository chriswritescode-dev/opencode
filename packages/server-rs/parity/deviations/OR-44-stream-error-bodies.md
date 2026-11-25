# OR-44: mid-stream error bodies are re-encoded instead of the original frame text

TypeScript: `packages/ai/src/route/client.ts:418-431` defines `frameError`, which replaces an `AIError` body that is absent or exactly equal to `encodeJson(event)` with `execution.body?.(frame)` or the raw frame text. A provider error frame therefore keeps its original whitespace, number spelling and escapes in `reason.body` even when the protocol re-serialized the parsed event (pinned commit `e54fbacb3d3aebc85f0e5ada0a5c490cc9333a91`).

Rust: `crates/ai/src/route/client.rs` `read_next` passes the frame to the protocol and attaches only `HttpContext` to the error; it never substitutes the frame text. Protocols build the error body themselves: `crates/ai/src/protocols/openai_chat.rs:74-83` and `crates/ai/src/protocols/gemini.rs:900-906` re-encode the parsed event with `crate::json::encode_body`, `crates/ai/src/protocols/anthropic_messages.rs:2028-2037` does the same, and Bedrock's `bedrock_converse.rs:1016-1027` re-encodes the object with `rawBody` removed. The body is therefore the canonical serialization rather than the provider's bytes.

Reason: the Rust protocols do not retain the raw frame on the error path, so the body is whatever their JSON encoder produced. The difference is only visible in `reason.body` for a mid-stream failure, and no corpus cassette fails mid-stream; `crates/ai/tests/all/replay.rs` compares completed streams and their error-free events, so replay does not catch it.

Accepted: pending owner
