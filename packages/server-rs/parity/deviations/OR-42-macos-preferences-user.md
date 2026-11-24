# OR-42: managed preferences take the user name from `$USER` instead of the passwd entry

TypeScript: `ConfigDiscovery.discover` builds the macOS MDM preference paths from `os.userInfo().username`, falling
back to `"user"` when that lookup fails, and keeps the first path that is a file
(`packages/core/src/config/discovery.ts:144` at the pinned commit
`73a0eae88de47f10068a0f9dc7a743638fa3d830`).

Rust: `crates/config/src/discovery.rs` `preferences` takes the name from `std::env::var("USER")`, falls back to
`"user"`, and passes it to `preference_paths`, which checks the per-user plist first and the machine-wide one second
(`:185-194`, `:163-166`).

Reason: the preference path is macOS-only code, the crate has no passwd lookup dependency, and it cannot be
validated in the Linux CI or the sandbox. The two differ only when `USER` is unset or overridden relative to the
passwd entry.

Accepted: pending owner
