# OR-44: compaction and checkpoint requests are built but never executed

TypeScript: `packages/ai/src/protocols/openai-responses.ts:424-431` gives the route both `compact.endpoint` and `compact.trigger`. The endpoint operation in `packages/ai/src/protocols/utils/responses-compaction.ts:128-161` POSTs the validated `/compact` body, reads the response text and decodes `CompactionResponse`, requiring a `compaction` output item. The trigger operation in `packages/ai/src/protocols/utils/responses-checkpoint.ts:108-112` prepares a streaming route, runs it to completion and collects exactly one checkpoint (pinned commit `e54fbacb3d3aebc85f0e5ada0a5c490cc9333a91`).

Rust: only the request builders exist. `crates/ai/src/protocols/utils/responses_compaction.rs` `compact_request` returns a URL and body, and `crates/ai/src/protocols/utils/responses_checkpoint.rs` `checkpoint_request` appends the `compaction_trigger` input item and returns a URL and body. Neither has a caller in `crates/ai/src`; both are exercised only by `crates/ai/tests/all/protocols.rs`, which asserts the built bytes. There is no POST, no response decode and no checkpoint collection. OR-45 adds `xai_responses::compact_request` (`crates/ai/src/protocols/xai_responses.rs:110-112`), which uses the xAI adapter and lowers tools the same way, mirroring `packages/ai/src/protocols/xai-responses.ts:103-104`; like the OpenAI builder, nothing executes it.

Reason: the LLM layer port stopped at request construction and no execution path was ported. Until one exists a caller cannot compact or checkpoint, and no replay cassette covers the request because the server never sends it. The OpenAI request builders are covered byte-for-byte by the tests above so a future executor can reuse them.

Accepted: pending owner
