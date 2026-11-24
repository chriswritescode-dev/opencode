# OR-42: config watching ignores directories per event and uses one native watcher per target

TypeScript: `ConfigWatch.plan` puts `ignore: FSUtil.ignoreGlobs` on every directory target
(`packages/core/src/config/watch.ts:64` at the pinned commit `73a0eae88de47f10068a0f9dc7a743638fa3d830`), and
`@parcel/watcher` applies that list natively, so ignored subtrees are never watched.
`FilesystemWatcher.subscribe` opens the subscription through an `RcMap` keyed by target, type, ignore and names
(`packages/core/src/filesystem/watcher.ts:138`), so one host subscription serves every subscriber of the same target.

Rust: `crates/config/src/watch.rs` `Watcher` creates one `notify::RecommendedWatcher` per target (`:135-191`) and
watches a `Directory` target with `RecursiveMode::Recursive` (`:182-189`); the ignore globs are carried on the target
but only filter events after they arrive, in `matches` (`:202-212`). inotify therefore takes watch slots for ignored
subtrees, and every target holds its own inotify instance.

Reason: `notify` has no native ignore list, and a shared, ignore-aware watcher belongs with the filesystem watcher
port rather than the config crate. The shared watcher is deferred in the OR-42 review.

Accepted: pending owner
