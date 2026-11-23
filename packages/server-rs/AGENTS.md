# server-rs

The Rust replacement for the TypeScript OpenCode server process, the `opencode-server` binary. The root
`AGENTS.md` applies too.

## Build

- Run cargo through the package script, for example `bun run --cwd packages/server-rs cargo check -p opencode-server`.
- `rust-toolchain.toml` pins the toolchain and must equal `packages/cli-rs/rust-toolchain.toml`.
- This is its own Cargo workspace and never joins cli-rs's, and no root `Cargo.toml` is added.
- `opencode-local` and `opencode-util`, and `opencode-service` and `opencode-api` as test clients, come from
  `packages/cli-rs` by path and are never edited from here.
- `opencode-api-types` also comes from `packages/cli-rs` by path, but is used at runtime only for its `js` helpers
  (`number_to_string`, `parse_number`, `encode_component`, `decode_component`); the server never uses its generated
  wire types.
- This workspace has its own `Cargo.lock`, so the cli-rs crates build here against dependency versions that can
  differ from `packages/cli-rs/Cargo.lock`; update it on its own schedule with `bun run cargo update`.

## Layout

- Crate directories are `crates/<name>` with package name `opencode-<name>`.
- Declare every crate and third-party dependency once in `[workspace.dependencies]`.
- Each task adds the crate it needs, following the indicative split in project.md `Modules.md`: contract, storage,
  bus, config, location, ai, catalog, tool, fs, shell, vcs, session, mcp, sidecar, plugin, http and server, plus
  `host/` for the sidecar's TypeScript and `parity/`.
- The wire contract is the generated `opencode-contract` crate (`crates/contract`): the pinned OpenAPI document plus
  the generated route table, types, events, errors, request decoders and corpus. It is generated, never edited by
  hand.
- Generators read the pinned TypeScript sources through `script/upstream.ts`, which materializes
  `target/upstream/<commit>` from `git archive` at the commit in `upstream-pin`; they never read the working tree,
  which can be newer.
- `bun run drift:report <commit>` from this package compares the pin with a target commit and reports changed
  operations, component schemas, events, durable members and migrations. Its `git diff` pathspecs are relative to
  the working directory, so run it from `packages/server-rs` or prefix paths with `:/`.

## Checks

Before merging, the whole package must pass:

- `bun run cargo fmt --check`
- `bun run cargo clippy --locked --workspace --all-targets -- -D warnings`
- `bun run cargo test --locked --workspace --no-fail-fast`
- `bun run check:deny`
- `bun run check:drift`

Each crate keeps one integration test binary, `tests/all/main.rs`, with one module per area. Add tests as a module
there, not as a new `tests/*.rs`.

## Rules

- Generated files are committed and never edited by hand. Regenerate them with the generator under `script/gen/`;
  `bun run check:drift` fails when one is stale.
- Every difference from the TypeScript server needs a file in `parity/deviations/` that the owner has accepted. A
  difference without one is a failure.
- All work targets the upstream commit in `upstream-pin`. Read the TypeScript sources at that commit, for example
  `git show $(cat upstream-pin):packages/protocol/openapi.json`, whenever the checkout is newer. Only re-sync
  tasks change the pin.
- A new third-party dependency or licence needs `deny.toml` to pass and is recorded there with the crates that need
  it.
- First-attempt code is copied only by the task that `docs/salvage.md` names for it.
