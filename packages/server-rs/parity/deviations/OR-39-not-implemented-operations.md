# OR-39: a contract operation without a handler answers 501 instead of TypeScript's implementation

TypeScript: the server implements every contract route. `packages/server/src/routes.ts:110-123` builds the full
route table through `createRoutes`, and `packages/server/src/handlers.ts` registers a handler group for each route,
so any advertised operation has a working endpoint. The machine-link WebSocket upgrade
(`/api/experimental/machine/link`) is exempt from password authentication while the `machines` flag is on
(`packages/server/src/process.ts:226`, `packages/protocol/src/groups/machine.ts:74-80`).

Rust: `crates/http/src/app.rs:19` lists the implemented operations in `IMPLEMENTED`
(`server.info`, `server.pair`, `server.connect`, `event.subscribe`, `experimental.session.log`, `location.get`,
`location.reload`, `debug.heapDump`, `debug.location.list`, `debug.location.evict`, `project.list`, `project.update`,
`vcs.init`, `vcs.get`, `vcs.base`, `vcs.status`, `vcs.branch.list`, `vcs.diff`, `config.get`, `config.shells`,
`experimental.config.update`). That list grows as handler tasks land. `not_implemented`
in `crates/http/src/app.rs:281` answers every other matched route with status `501 Not Implemented` and an
`UnknownError` body whose message is `opencode-server does not implement <identifier> yet`. `docs/coverage.md` is
generated from `IMPLEMENTED` (`OPENCODE_UPDATE_COVERAGE=1`) and marks every route `implemented` or
`not implemented (501)`. `exempt` in `crates/http/src/app.rs:297` exempts only the pairing connect link and ticketed
connect routes, so the machine-link upgrade requires credentials and answers 401 before the 501 body.

Reason: the remaining routes belong to the tasks named in the OR-39 plan and will land on the same router, so the
router stays complete while handlers arrive incrementally. `crates/http/tests/all/coverage.rs` checks the coverage
document against `IMPLEMENTED`, `crates/http/tests/all/status.rs:145` checks the 501 body, and
`crates/http/tests/all/status.rs:157` checks that every ported operation is listed. The task implementing
`machine.link` restores the unauthenticated upgrade exemption.

Accepted: pending owner
