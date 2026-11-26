# OR-48: Console status retries, poll timing and watch recovery differ

TypeScript: `SharedCache` retries only a timeout, a transport error and HTTP 408, 429, 500, 502, 503 or 504
(`packages/core/src/shared-cache.ts:38-44`). A Console 403 that is not the SSO error, or any other 4xx, is returned at
once. The periodic Console check is one fiber started at plugin init and after an account switch
(`opencode.ts:575-581`); it sleeps one minute between checks. `syncWatches` catches a failed credential resolution or
server normalisation, logs it, and leaves the current watches running (`opencode.ts:347-366`).

Rust: `crates/catalog/src/provider_plugin/opencode/remote.rs:124` and `:180` wrap any other 401 or 403/4xx status in
`ConsoleError::Transient`, and `is_transient` at `:77-81` returns true for every `Transient`, so
`crates/catalog/src/shared_cache.rs:139-150` retries it three times. Skill downloads map `FollowError::Status` to the same
transient status error (`content.rs:222-232`), so they retry by the same rule. The poll supervisor
(`opencode/mod.rs:202-236`) restarts its one-minute sleep whenever `ConsoleHandle::bump` (`handle.rs:241`) publishes a
new revision, so any applied snapshot delays the next check. `sync_watches`/`watch_target`
(`opencode/mod.rs:513-559`) return no target on a resolve or normalise failure, which stops the watches.

Reason: the Rust cache has one `Transient` marker instead of Effect's status list, the poll loop is a single supervisor
task rather than a `FiberHandle`, and the watch hub is a process task rather than an Effect lease. The ported tests cover
the poll calling `check`, the long-poll fallback and the reconnect behaviour; none measures request counts or timer
phases. A non-SSO 403 therefore costs four requests instead of one.

Accepted: pending owner
