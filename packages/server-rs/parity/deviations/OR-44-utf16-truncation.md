# OR-44: id and name limits count Unicode scalars instead of UTF-16 code units

TypeScript: `packages/ai/src/protocols/openai-chat.ts:682` truncates an OpenAI tool-call id with `id.slice(0, 40)`, which cuts UTF-16 code units and can split a surrogate pair into a lone high surrogate. `packages/ai/src/protocols/bedrock-converse.ts:315-316` truncates a replayed tool name with `part.name.slice(0, 64)` the same way, and `packages/ai/src/protocols/utils/bedrock-media.ts:61-77` limits a document label with `.slice(0, 200)` (and `200 - suffix.length` when disambiguating) in code units. `packages/ai/src/protocols/utils/mistral-tool-id.ts:5-11` hashes each code point with `char.charCodeAt(0)`, which is the first UTF-16 unit, so an astral character contributes its high surrogate value (pinned commit `73a0eae88de47f10068a0f9dc7a743638fa3d830`).

Rust: `crates/ai/src/protocols/openai_chat.rs:805-806` truncates with `id.chars().take(40)`, `crates/ai/src/protocols/bedrock_converse.rs:589` with `part.name.chars().take(64)`, and `crates/ai/src/protocols/utils/bedrock_media.rs` `document_name` (lines 95-125) counts `chars()` for the 200 limit and the suffix budget. `crates/ai/src/protocols/utils/mistral_tool_id.rs` `seed_hash` (lines 43-47) folds `character as u32`, so an astral character contributes its full scalar value.

Reason: Rust strings are UTF-8 and cannot hold a lone surrogate, so `.chars()` is the only valid truncation; this also avoids producing invalid text. The two agree for the entire BMP and differ only when a limit falls inside, or a hash consumes, a non-BMP character. The request corpus contains astral characters only in positions that do not cross these boundaries, so `crates/ai/tests/all/replay.rs` does not distinguish the behavior.

Accepted: pending owner
