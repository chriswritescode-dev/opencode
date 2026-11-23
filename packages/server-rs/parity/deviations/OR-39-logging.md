# OR-39: log records go to stderr and no log file is written

TypeScript: `Observability.layer` installs `Logging.loggers`, writing every record to
`<Global.Path.log>/opencode-<channel>.log` (or `opencode.log` for release channels) and optionally mirroring to
stderr: `packages/util/src/observability/logging.ts:59-77` and `packages/util/src/observability.ts:31`.

Rust: `opencode-server` writes startup and shutdown messages, including the managed-service listening line and
ownership warnings, directly to stderr, in `crates/server/src/serve.rs` and `crates/server/src/service_mode.rs`. It
creates no log file under `<data>/opencode/log/`.

Reason: the server does not depend on `opencode-core` or a structured logger yet; the configuration and paths task
(OR-42) owns the log directory and logging layer.

Accepted: pending owner
