# OR-42: a migrated legacy entry that fails re-decoding is dropped with an invalid diagnostic

TypeScript: `ConfigNormalize.canonical` re-decodes and re-encodes a migrated legacy entry with
`Schema.decodeUnknownOption(...).pipe(Option.flatMap(...))` and unwraps the result with `Option.getOrThrow`, so a
migrated value the current schema rejects throws out of `normalize` and the whole configuration load fails
(`packages/core/src/config/normalize.ts:766-774` at the pinned commit
`e54fbacb3d3aebc85f0e5ada0a5c490cc9333a91`). It is called for migrated commands (`:120-122`), agents (`:134-136`),
mode agents (`:152-154`), one V1 MCP server (`:311-313`) and each migrated provider (`:603-609`).

Rust: `crates/config/src/normalize.rs` `canonical` returns `None` when the re-decode or re-encode fails, and the
callers report the dropped entry with an `invalid` diagnostic and continue: `map_values` for commands, agents and mode
agents, `migrate_legacy_mcp` for a V1 MCP server, and `crates/config/src/normalize/v1.rs` `migrate_providers` for a
provider. The path mirrors the source map, so `{"provider":{"x":{"options":{"tls":1}}}}` reports `skipped malformed
recognized value` at `["provider", "x"]` and yields no provider, where the pinned TypeScript throws.

Reason: the Rust configuration load is shared by the whole process and one malformed legacy entry must not abort it,
so the entry is still dropped but no longer silently. `script/gen/config-corpus.ts` exports a normalize case for this
input, and that case is the parity check for the diagnostic.

Accepted: pending owner
