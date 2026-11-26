# OR-50: reference registry and config references

TypeScript: `packages/core/src/reference.ts` owns `Reference.Service`, whose `list()` projects each configured source into `Reference.Info`, whose notify callback forks `refresh()` and publishes `reference.updated`, and whose scoped fiber repeats `refresh()` hourly through `RepositoryCache.ensure`. `packages/core/src/config/plugin/reference.ts` resolves `references` entries against the document directory or the location directory and expands `~/`. `packages/core/src/reference/instructions.ts` renders the `core/reference-guidance` instruction.

Rust: `crates/registry/src/reference.rs` owns `ReferenceService` with the same `State` data (`IndexMap<name, Source>`), editor, `list()` projection, `reference.updated` notification and hourly refresh timer. `crates/registry/src/config/reference.rs` ports the config loader and reloads with `opencode_config::observer::observe`. `crates/registry/src/reference/instructions.rs` ports the initial guidance render, including the empty/removed case. `crates/registry/src/repository.rs` ports the pure repository functions for OR-53 to reuse.

Known differences that remain inside Phase 9:

- **GitHub clone base override.** `repository::github_remote` always builds `https://github.com/<path>.git`. TypeScript reads `OPENCODE_REPO_CLONE_GITHUB_BASE_URL` and resolves against it. The process environment reaches registry services only through the composition wired in Phase 13, so OR-53 must thread that variable through `repository::parse` when it adds clone work.
- **Refresh dispatch.** The notify callback calls `ReferenceRefresh::refresh` synchronously on the notifying task and the hourly task calls it on the timer task. TypeScript forks `refresh()` into the service scope and runs `RepositoryCache.ensure` with concurrency 4. The default `NoRefresh` logs at debug level and does nothing; OR-53 supplies the repository-cache implementation.
- **Refresh filtering.** `refresh_sources` re-parses each configured git source and validates its branch, matching the filtered `list()` TypeScript re-reads. Invalid remotes and branches are skipped silently, as they are in `list()`.
- **Guidance updates.** Only the initial `core/reference-guidance` render and the empty/removed case are ported; changed/removed update rendering is deferred to OR-55 with the instruction-update deviation.
- **String ordering.** Guidance summaries sort with Rust byte order instead of JavaScript `localeCompare`. This is identical for the ASCII names the corpus covers.
- **Wire source fields.** The pinned OpenAPI `Reference.LocalSource` and `Reference.GitSource` components omit `description` and `hidden`, so `Reference.Info.source` carries neither. `Reference.Info` itself still carries both fields and they remain part of the change projection.

Accepted: pending owner
