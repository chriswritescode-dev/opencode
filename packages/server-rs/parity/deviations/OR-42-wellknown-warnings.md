# OR-42: well-known source failures are silent instead of logged

TypeScript: `packages/core/src/wellknown.ts:126-145` at the pinned commit
`73a0eae88de47f10068a0f9dc7a743638fa3d830` logs `failed to load wellknown manifest` when an unreachable source
falls back to its cached manifest, and `skipping wellknown source` when `loadSource` drops the failure, both through
Effect logging with the origin and error.

Rust: `crates/config/src/wellknown.rs` returns the original inspect error from `load_entry` or swallows it to `None`
in `load_source` without logging. The config crate has no logger, so neither failure produces a diagnostic line.

Reason: diagnostics belong to the logging layer (OR-39). The well-known tests cover the cached-manifest fallback and
the skipped-unreachable-source behaviour without the log lines.

Accepted: pending owner
