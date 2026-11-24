# OR-41: query decode failures carry the contract decoder's message text instead of Effect's schema formatter

TypeScript: the HTTP API validates `GET /api/event` through `EventSubscribeQuery`, the `Schema.Struct` defined at
`packages/protocol/src/groups/event.ts:38-53`, and its query is built at `packages/protocol/src/groups/event.ts:61-68`.
A decode failure reaches `schemaErrorLayer` (`packages/server/src/middleware/schema-error.ts:15-49`), which truncates
`error.cause.message` (`packages/server/src/middleware/schema-error.ts:10-13`) to 1024 characters and appends
`... (<N> more chars)`. Effect's formatter renders the reason
across lines with the failing path below it, for example `Expected an integer\n  at ["after"]`, and for a literal
union it prints the list of accepted literals. The layer answers `400` with an `InvalidRequestError` whose `_tag` is
`InvalidRequestError`, whose `message` is that formatted reason, and whose `kind` is the decode location
(`packages/protocol/src/errors.ts`).

Rust: `crates/http/src/handlers/event.rs` calls `event_subscribe::decode_query` from the generated contract and, on
`Err`, returns `crates/http/src/handlers/invalid.rs::request` with `Kind::Query`. That handler answers `400` with
the same `_tag` and `kind: "Query"` fields, but `message` is `DecodeError`'s display text from
`crates/contract/src/codec.rs:15-19`, which is a single `<path>: <reason>` line such as `$.types[0]: no member
matched` and is never truncated.

Reason: the Rust server decodes requests with the generated contract decoder rather than Effect's `Schema`, so it has
no Effect formatter output to reuse. Status, `_tag` and `kind` stay identical and only the human-readable `message`
text differs; clients that branch on the error tag or the status code see no change. `crates/http/tests/all/coverage.rs`
keeps `event.subscribe` listed as implemented, and the OR-41 HTTP layer tests cover the 400 body.

Accepted: pending owner
