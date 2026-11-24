# OR-43: a project.list query decode failure carries the contract decoder's message text

TypeScript: `packages/protocol/src/groups/project.ts:8-18` decodes the `archived` query with `BooleanFromString`, a
`Schema.Literals(["true", "false"])`. A decode failure reaches `schemaErrorLayer`
(`packages/server/src/middleware/schema-error.ts:15-49`), which answers `400` with an `InvalidRequestError` whose
`message` is Effect's formatted reason, `Expected "true" | "false"\n  at ["archived"]`, and whose `kind` is `Query`.

Rust: `crates/http/src/handlers/project.rs::list` calls `project_list::decode_query` from the generated contract and, on
`Err`, returns `crates/http/src/handlers/invalid.rs::request` with `Kind::Query`. Status, `_tag` and `kind` match, but
`message` is the decoder's display text from `crates/contract/src/codec.rs`, `$.archived: expected a boolean`.

Reason: the same root cause as `OR-41-query-decode-messages.md`. The Rust server decodes requests with the generated
contract decoder rather than Effect's `Schema`, so it has no Effect formatter output to reuse; only the human-readable
`message` differs, and clients that branch on the tag or status see no change.
`parity/scenarios/project-routes.scenario.ts` step 6 (`project.list?archived=maybe`) is the oracle check for the 400 and
records the difference.

Accepted: pending owner

```parity
project-routes steps/5/body/message
```
