# OR-42: `{file:~/...}` expands from the configured home instead of `os.homedir()`

TypeScript: `ConfigVariable.substituteFiles` expands a leading `~/` in a `{file:...}` reference with `os.homedir()`
and resolves the result against the config directory (`packages/core/src/config/variable.ts:63` at the pinned commit
`73a0eae88de47f10068a0f9dc7a743638fa3d830`).

Rust: `crates/config/src/variable.rs` `substitute_files` expands `~/` from `SubstituteInput.home` (`:83-86`), which
is `GlobalPaths.home` from the crate's path configuration. As OR-42's plan Phase 5 requires, `GlobalPaths` honours
`OPENCODE_TEST_HOME` (`packages/cli-rs/crates/local/src/paths.rs:48`).

Reason: the crate takes every path from the injected `GlobalPaths` so callers and tests can redirect them, and the
two differ only when `OPENCODE_TEST_HOME` is set.

Accepted: pending owner
