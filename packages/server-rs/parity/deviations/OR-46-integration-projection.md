# OR-46: integrations sort by byte order and change detection ignores connections and statuses

TypeScript: `Integration` registers `State.changes` with a projection that maps every entry through
`project(entry, resolveConnections(entry, []))` and sorts the results with `(a, b) => a.name.localeCompare(b.name)`,
publishing `integration.updated` with `{}` whenever the JSON of that projection changes
(`packages/core/src/integration.ts:359-365` at the pinned commit `73a0eae88de47f10068a0f9dc7a743638fa3d830`).
`project` includes the resolved connections and merges each connection's process-local status from `statuses`
(`packages/core/src/integration.ts:405-415`), while `list` sorts entries by `ref.name.localeCompare` (`:373`). The
empty saved-credential list passed to `resolveConnections` means the projection carries only connections derived from
integration `env` methods, not stored credentials (`:389-403`).

Rust: `IntegrationService` registers `state::changes(project_registry, notify)`; `project_registry` clones each
`IntegrationInfo` with `connections: Vec::new()` and sorts with `a.name.cmp(&b.name)` before encoding, so only `id`,
`name`, `methods` and `metadata` drive the change notification (`crates/catalog/src/integration.rs:302-310`), and
`list` sorts with the same `cmp` (`crates/catalog/src/integration.rs:227`). A connection status change publishes
`integration.updated` directly from the status method (`crates/catalog/src/integration/connection.rs:140-152`). The
service subscribes to no credential events, matching the TypeScript integration service, which also subscribes to
none.

Reason: `String::cmp` is UTF-8 byte order, which differs from `localeCompare` only for names containing non-ASCII
characters. The narrower projection avoids an extra `integration.updated` when a status change, already published by
the status method, is followed by a transform, and it ignores env-derived connection changes; the event type and the
`{}` payload are identical. `integration-routes` and `credential-routes` compare HTTP response bodies and database
rows and do not assert event counts, so neither scenario observes the difference.

Accepted: pending owner
