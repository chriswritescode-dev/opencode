# OR-39: server info always reports the persistent PTY capability as unavailable

TypeScript: `packages/server/src/handlers/server.ts:27` reports `capabilities.persistentPty` from
`PersistentPty.available`, so the flag reflects whether the running server can hold a persistent terminal session
for a client.

Rust: `crates/http/src/app.rs` `server_info` encodes `persistent_pty: false` for every build. No `opencode-http`
build can serve a persistent PTY until OR-52 adds the PTY service. `crates/server/src/service/stop.rs` also stops a
service without the handoff or clear steps that `Service.stop` and `ensure` run in
`packages/client/src/effect/service.ts:130-136`, because there is no PTY handoff to preserve or move.

Reason: `opencode-http` has no PTY service to query, and advertising a capability it cannot serve would make clients
open a socket the server cannot answer. `crates/http/tests/all/info.rs:47` checks the false value; OR-52 replaces it
with real availability.

Accepted: pending owner
