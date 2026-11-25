# OR-42: the sources observer applies its initial load instead of returning it

TypeScript: `ConfigEntryObserver.observeSources` loads the documents once before returning, stores them in
`loaded.documents` and does not invoke `input.reload` for that first scan; the caller registers the returned
documents itself, and `input.reload` runs only from the 100 ms debounced trigger loop
(`packages/core/src/config/plugin/entry-observer.ts:42-70` at the pinned commit
`e54fbacb3d3aebc85f0e5ada0a5c490cc9333a91`).

Rust: `crates/config/src/observer.rs`'s `observe_sources` performs the same pre-subscription scan but hands it to
`apply` synchronously before returning, then calls `apply` again after every debounced reload. `ObserverHandle` is
not generic over the loaded document type, so the initial value cannot be returned to the caller. The apply/load
contract is the only channel for the initial registration.

Reason: Rust callers register through `apply`, which is the reload action itself, and the separate initial
registration step has no place in that shape. The `observer::observe_sources_applies_the_initial_load_and_fires_for_a_directory_change`
test covers the initial apply, so registering through `apply` must be idempotent in each domain loader.

Accepted: pending owner
