# OR-39: diagnostic lines go to stderr and no log file is written

TypeScript: `Observability.layer` installs `Logging.loggers`, writing every record to
`<Global.Path.log>/opencode-<channel>.log` (or `opencode.log` for release channels) and optionally mirroring to
stderr: `packages/util/src/observability/logging.ts:59-77` and `packages/util/src/observability.ts:31`. The
service's `server listening on <url>` and `server password <password>` lines are written with `console.log` to
stdout, not through the logger: `packages/cli/src/server-process.ts:213-215`.

Rust: `opencode-server` writes warnings, errors and shutdown messages to stderr in `crates/server/src/serve.rs`,
`crates/server/src/service_mode.rs` and `crates/server/src/shutdown.rs`. The managed-service listening line goes to
stdout through `serve::write_line`, matching the TypeScript `console.log`. It creates no log file under
`<data>/opencode/log/`.

Reason: the server does not depend on `opencode-core` or a structured logger yet; the configuration and paths task
(OR-42) owns the log directory and logging layer. No check yet for the missing log file;
`crates/server/tests/all/serve.rs` and `crates/server/tests/all/service_mode.rs` cover the stdout listening lines
and the stderr shutdown and ownership warnings.

Accepted: pending owner
