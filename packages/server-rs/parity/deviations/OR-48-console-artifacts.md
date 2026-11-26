# OR-48: the Console artifact client normalises the server and uploads in fixed batches

TypeScript: `sender` builds each URL by appending the path to `ConsoleAccess.server(credential)` unchanged
(`packages/core/src/plugin/provider/opencode-artifacts.ts:180`), so a stored server keeps its trailing slash, query and
fragment. Uploads and reads run with Effect's `{ concurrency: 4 }` (`:65`, `:123`), a sliding window that starts the next
request as soon as one finishes.

Rust: `crates/catalog/src/provider_plugin/opencode/artifacts.rs:253` builds the base with
`console_access::normalize_server`, which keeps the origin and trimmed path, drops the query and fragment, and rejects a
server that is not HTTP(S) or does not parse. The same module uploads and reads in fixed chunks of four
(`artifacts.rs:137-157`, `:190-211`), so a slow request in one chunk holds back the next.

Reason: normalisation is shared with sign-in, watch and skill URLs, and a server saved by sign-in is already normalised,
so the only visible difference is a hand-written server with a trailing slash or query; the Rust code has no Effect
concurrency primitive and a batch keeps the same maximum of four requests in flight. The ported artifact tests cover the
slug, the upload-only-missing rule, the revision download and the error mapping; no parity scenario publishes artifacts.

Accepted: pending owner
