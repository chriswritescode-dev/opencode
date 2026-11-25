# OR-62: a missing plugin target watches its nearest existing ancestor non-recursively

TypeScript: `watchTarget` (`packages/core/src/plugin/module.ts:69` at the pinned commit
`73a0eae88de47f10068a0f9dc7a743638fa3d830`) walks up to the nearest existing ancestor and subscribes to it, and a
directory target is watched recursively by `@parcel/watcher` (`packages/core/src/filesystem/watcher.ts:246`).

Rust: `crates/plugin/src/watch.rs` `watch_target` reports recursion only when the requested path itself exists as a
directory. A missing target watches its nearest existing ancestor with `RecursiveMode::NonRecursive`, which still sees
the requested path being created. The notify callback only reloads on create, modify, remove or rename and
`Access(Close(Write))`, ignoring `Err` and the open, read and read-close access events that `notify`'s inotify backend
reports.

Reason: a mistyped absolute plugin target such as `/opt/plugins/foo` made the ancestor fallback a recursive watch of
`/` or `$HOME`, exhausting inotify watches and reloading on unrelated writes. Reading the plugin files also produced
access events that retriggered the reload that caused the read.

Accepted: pending owner
