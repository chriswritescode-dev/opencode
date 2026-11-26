# OR-53: a snapshot capture retries when a file changes while Git indexes it

TypeScript: `packages/core/src/git.ts:873` (`captureLocked`) runs `attemptCapture` (`packages/core/src/git.ts:771`)
once and retries only after rebuilding an unreadable index. When another process rewrites a file between Git's
`lstat` and its read in `update-index --add` (`packages/core/src/git.ts:557`), Git exits with
`short read while indexing <file>` and `packages/core/src/snapshot.ts:146` logs the failure and returns no snapshot.

Rust: `attempt_capture_settled` in `crates/git/src/tree.rs:485` runs `attempt_capture` up to three times while the
refresh fails with `short read while indexing`. Each attempt uses a fresh temporary index, so a failed attempt leaves
nothing behind, and every other failure still ends the capture after the first attempt.

Reason: the failure is a race with a concurrent writer, not a property of the worktree, and the next attempt reads the
settled file. `snapshot::captures_concurrently_from_independent_processes` in `crates/vcs/tests/all/snapshot.rs`
(the port of the pinned `captures concurrently from independent processes` test, which expects every capture to
succeed) failed about once in 100 runs without the retry. No parity scenario covers the race.

Accepted: pending owner
