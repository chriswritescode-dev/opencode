# OR-42: an unreadable or unresolvable config file is skipped instead of failing the load

TypeScript: `Config.load` reads each existing config file through `FSUtil.readFileStringSafe`, which swallows only
`NotFound` and access-denied (`PermissionDenied` or `EPERM`) failures and returns `undefined` for those, rethrowing
everything else (`packages/util/src/fs-util.ts:63-69`); `load` runs the result through `Effect.orDie`
(`packages/core/src/config.ts:290-324`). `ConfigDiscovery.discover` resolves each candidate through `FSUtil.resolve`,
which maps the same two reasons to the unresolved path and `Effect.orDie`s every other error
(`packages/util/src/fs-util.ts:81-93`), and discovery itself is `Effect.orDie`d
(`packages/core/src/config/discovery.ts:120-124`). A failure such as `EISDIR`, `EIO` or `ELOOP` therefore fails the
whole load.

Rust: `crates/config/src/load.rs` `load_file` reads with `std::fs::read(path).ok()?` (`:234-245`), so any read error
skips the file, and `crates/config/src/discovery.rs` `resolve_path` falls back to the absolute path when `realpath`
fails (`:247-250`). `load` returns `Loaded` and has no error channel at all.

Reason: the Rust load is infallible and shared by the whole process, so one unreadable file must not take down
configuration. No corpus case covers a file that exists but cannot be read or resolved.
