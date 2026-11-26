# OR-51: watcher ignore globs are a subset, and Linux access events are filtered

TypeScript: `packages/core/src/filesystem/watcher.ts:246-281` delegates recursive `directory` watches, including their `ignore` patterns, to the `@parcel/watcher` native binding, which compiles the patterns with its own glob implementation.

Rust: `crates/fs/src/watcher.rs` watches recursively with `notify` and filters ignored paths itself. An ignore entry containing `*`, `?`, `[` or `{` is matched as a glob with `*`, `**` (any number of path segments) and `?`; other entries are resolved against the watched directory and ignored by containment. Character classes (`[...]`) and brace groups (`{a,b}`) are treated literally, so those upstream patterns do not match. `notify` access events are also dropped except close-for-write, where `@parcel/watcher` reports only create, update and delete.

Reason: the workspace has no glob dependency and the pinned watcher is `notify`; the subset covers every ignore value the pinned TypeScript passes to `Watcher` (`.git`, `.hg` and the configured location-watcher names). No parity scenario in this task exercises a recursive watcher with a character-class or brace ignore.

Accepted: pending owner

```parity
```
