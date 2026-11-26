# OR-51: formatter package binaries come from `node_modules/.bin`, not an npm install

TypeScript: `packages/core/src/formatter/builtins.ts:82,96,137` resolves prettier, oxfmt and `@biomejs/biome` through `Npm.which`, which picks the current generation under the global npm cache and, when nothing is installed, installs the package with `npm` before returning its binary.

Rust: `crates/fs/src/formatter/builtins.rs` looks for `<name>` in `node_modules/.bin` from the Location directory up to the worktree and enables the formatter only when it is already there. For `@biomejs/biome` it looks for the `biome` binary. This follows the Phase 7 instruction and keeps formatting from starting a package install while the server writes a file.

Consequences: a project that depends on prettier, oxfmt or biome but has not run its package manager yet is not formatted until the binary exists. A project that has no local copy is not formatted even if the TypeScript server would have installed one into its cache.

Also omitted: `packages/core/src/formatter.ts:59` skips targets whose environment does not host the server. The Rust `Target` has no environment, because every write in this crate acts on the server's own disk, so the check has no equivalent.

Accepted: pending owner

```parity
```
