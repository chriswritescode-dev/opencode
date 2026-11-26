# OR-51: fuzzy file search always uses the ripgrep index

TypeScript: `packages/core/src/filesystem/search.ts:119-239` selects the native `fff` picker (`@ff-labs/fff-*@0.10.5`) for a version-controlled Location that is not the home directory, and the ripgrep index path (`search.ts:53-117`) for every other Location.

Rust: `crates/fs/src/search/mod.rs` builds the ripgrep index for every Location and scores it with `crates/fs/src/search/fuzzysort.rs`, so a version-controlled non-home Location uses the ripgrep path too.

Reason: `fff` is a closed native scorer reachable only through FFI, and the workspace denies `unsafe_code`, so the native picker cannot be called from Rust. The Rust path reproduces `fuzzysort@3.1.0` and the ripgrep index, including the 100 000 entry bound and the protected-name excludes for the home directory. `crates/fs/tests/all/search.rs` and the `fuzzysort` and `search` corpora cover the ripgrep path; no parity scenario in this task runs `fs.find` in a git repository.

Accepted: pending owner

```parity
```
