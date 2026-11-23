# OR-39: only three contract operations are implemented and every other matched route answers 501

TypeScript: the server implements every contract route. `packages/server/src/routes.ts:110-123` builds the full
route table through `createRoutes`, and `packages/server/src/handlers.ts` registers a handler group for each route,
so any advertised operation has a working endpoint. The machine-link WebSocket upgrade
(`/api/experimental/machine/link`) is exempt from password authentication while the `machines` flag is on
(`packages/server/src/process.ts:226`, `packages/protocol/src/groups/machine.ts:74-80`).

Rust: `crates/http/src/app.rs:18` lists the three implemented operations in `IMPLEMENTED` (`server.info`,
`server.pair`, `server.connect`). `crates/http/src/app.rs:92-103` answers every other matched route with status
`501 Not Implemented` and an `UnknownError` body whose message is
`opencode-server does not implement <identifier> yet`. `docs/coverage.md` lists all 178 routes and their status.
`exempt` in `crates/http/src/app.rs` exempts only the pairing connect link and ticketed connect routes, so the
machine-link upgrade requires credentials and answers 401 before the 501 body.

Reason: the remaining routes belong to the tasks named in the OR-39 plan and will land on the same router, so the
router stays complete while handlers arrive incrementally. `crates/http/tests/all/coverage.rs` checks the coverage
document and `crates/http/tests/all/status.rs:77` checks the 501 body. The task implementing `machine.link` restores
the unauthenticated upgrade exemption.

Accepted: pending owner
