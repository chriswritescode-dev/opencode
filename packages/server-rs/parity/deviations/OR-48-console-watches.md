# OR-48: watch state, session lookup, hooks and tool results use the Rust seams instead of the plugin context

TypeScript: the plugin stores watch state through `ctx.storage`, which `packages/core/src/plugin/host.ts:663-671` namespaces
with `plugin:<hex plugin id>:` over the shared KV store. That covers `watch-streak:`, `watch-deleted:` and
`watch-unattended:` (`packages/core/src/plugin/provider/opencode-watches.ts:117-119`) and `artifact-slug:`
(`opencode-artifacts.ts:33`). A wake checks `ctx.session.get` and treats only `Session.NotFoundError` as missing
(`opencode-watches.ts:123-127`). A deleted session's cancel failure propagates and the listener logs
`failed to cancel a deleted session's watches` (`:241-254`). The `prompt`, `execute.after` and `session.deleted` hooks
live in the watch lease (`:206-254`), and the shell-hook watch is forked in that lease (`:236`). The tools return
`{ output, content }` (`:325-326`, `:344`, `:357-358`, `:391-392`).

Rust: `crates/catalog/src/provider_plugin/opencode/watches/` uses the shared `opencode_config::seams::KvStore` directly, so
`watch-streak:<id>`, `watch-deleted:<id>`, `watch-unattended:<id>` (`wake.rs:81-91`, `tools.rs:127`) and
`artifact-slug:<id>` (`artifacts.rs:286-288`) are unprefixed keys in the same table. `SessionAccess::session` returns
`Option`, so a backend failure and a missing session both make `wake.rs:100` treat the session as absent. `forget`
drops a cancel failure (`wake.rs:78`) instead of logging it. The prompt, tool-execute and deletion hooks are registered
once per Location at plugin registration (`opencode/mod.rs:239-276`), so a prompt clears `watch-streak` even when no watch
lease exists, and the shell-hook create is a `tokio::spawn` (`tools.rs:343`) that is not aborted with the lease. The
`WatchTools` methods (`tools.rs:98-192`) return only the content string, `minutes` is checked with an invented
`minutes must be between 1 and 8640` message (`tools.rs:115-119`) instead of the tool's input schema, and the 403 notice
stores `description: ""` (`tools.rs:366`) where TypeScript omits it. In production `SessionAccess::synthetic` defaults to
`Unavailable` (`provider_plugin/mod.rs:163-165`) and `crates/http/src/services.rs:204` uses `NoSessions`, so watched
sessions cannot be woken until OR-54.

Reason: OR-54 supplies the real session and tool seams, and the plugin context has no per-plugin storage wrapper or
lease. Keys are consistent within Rust but a store shared with the TypeScript server sees different names. The ported
watch tests drive the seam through a recording `SessionAccess` and a memory KV, so the wake policy, labels and shell
target are covered; no parity scenario runs event watches.

Accepted: pending owner
