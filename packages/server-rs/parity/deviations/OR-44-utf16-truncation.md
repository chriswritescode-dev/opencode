# OR-44: id and name limits count Unicode scalars instead of UTF-16 code units

TypeScript: `packages/ai/src/protocols/openai-chat.ts:682` truncates an OpenAI tool-call id with `id.slice(0, 40)`, which cuts UTF-16 code units and can split a surrogate pair into a lone high surrogate. `packages/ai/src/protocols/bedrock-converse.ts:315-316` truncates a replayed tool name with `part.name.slice(0, 64)` the same way, and `packages/ai/src/protocols/utils/bedrock-media.ts:61-77` limits a document label with `.slice(0, 200)` (and `200 - suffix.length` when disambiguating) in code units. Effect's SSE parser (`effect/encoding/Sse`), used by `packages/ai/src/route/framing.ts`, limits a pending event to 10 MiB counted in UTF-16 code units (pinned commit `73a0eae88de47f10068a0f9dc7a743638fa3d830`).

Rust: `crates/ai/src/protocols/openai_chat.rs:805-806` truncates with `id.chars().take(40)`, `crates/ai/src/protocols/bedrock_converse.rs:589` with `part.name.chars().take(64)`, and `crates/ai/src/protocols/utils/bedrock_media.rs` `document_name` (lines 95-125) counts `chars()` for the 200 limit and the suffix budget. `crates/ai/src/route/framing.rs` counts the same 10 MiB SSE limit (`MAX_EVENT_SIZE`) in UTF-8 bytes, so an event of mostly non-ASCII text fails up to three times sooner.

Reason: Rust strings are UTF-8 and cannot hold a lone surrogate, so `.chars()` is the only valid truncation; this also avoids producing invalid text. The two agree for the entire BMP and differ only when a limit falls inside a non-BMP character. The request corpus contains astral characters only in positions that do not cross these boundaries, so `crates/ai/tests/all/replay.rs` does not distinguish the behavior.

Accepted: pending owner
