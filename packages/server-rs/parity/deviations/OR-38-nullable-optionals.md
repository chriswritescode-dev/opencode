# OR-38: an explicit null optional field decodes as absent

TypeScript: `HttpApi` encodes JSON bodies through `Schema.toCodecJson`. For an optional field built with the helper at `packages/schema/src/schema.ts:18-24` (used throughout `packages/schema/src`, for example `packages/schema/src/tool.ts:100`), `toCodecJson` rejects an explicit `null` with "Expected string", while an optional field built with Effect's plain `Schema.optional`, such as `packages/protocol/src/groups/event.ts:15-17`, accepts `null` and drops the key. The pinned `packages/protocol/openapi.json` marks every optional property as `anyOf [<schema>, null]` and outside `required`, so the published contract permits `null` for both spellings.

Rust: `crates/contract/src/codec.rs` `decode_optional_present` returns `Ok(None)` for a present `null`, and `write_optional` omits `None`. Every generated `Option<T>` field shares that one path, so the Rust decoder accepts `null` and the encoder writes the absent spelling.

Reason: JSON has no `undefined`, and `Schema.toCodecJson` writes an explicitly undefined optional as `null`, which has no JSON representation that a decoder can tell apart from an absent key. A single `Option<T>` cannot record present-`null` apart from absent, and the pinned OpenAPI declares both spellings valid, so the Rust codec accepts either on input and emits the absent spelling on output. The round-trip corpus and `crates/contract/tests/all/events.rs::an_explicit_null_optional_field_is_absent` cover it.

Accepted: pending owner
