# OR-50: skill discovery bounds each HTTP request

TypeScript: `packages/core/src/skill/discovery.ts` sends the index and file requests through the app `HttpClient` with no
explicit timeout. In the server runtime the Bun `fetch` behind it aborts an idle request after its default deadline,
about five minutes (`packages/util/src/no-timeout-fetch.ts`).

Rust: `crates/registry/src/skill/discovery.rs` applies `REQUEST_TIMEOUT` (5 s) to every attempt in `fetch`, the same
bound the configured instruction URL fetch uses in `crates/registry/src/config/instruction.rs`. `reqwest` has no
default request timeout, so a server that accepts a connection and never responds would block location boot
indefinitely, because `builtin::activate` awaits `config::skill::register`. Each of the three attempts is bounded, and a
timeout still retries with backoff like the other transient failures.

Accepted: pending owner
