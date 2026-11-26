# OR-46: provider-only keys in a model's own settings survive, and a failed prepare is logged

TypeScript: `modelSettings` omits every key that is present in `Provider.Settings` but absent from `Model.Settings` —
`timeout`, `headerTimeout`, `chunkTimeout`, `transport` and `tls` — and it is applied to the model's own settings as
well as the provider's before the two are merged (`packages/core/src/provider.ts:228-239`, merge at
`packages/core/src/model.ts:217-220` at the pinned commit `73a0eae88de47f10068a0f9dc7a743638fa3d830`). A failed
`providers.snapshot()` in `prepare` fails the model transform, because `transform` is
`prepare.pipe(Effect.andThen(state.transform(update)))` (`packages/core/src/model.ts:257-265`, `:285`).

Rust: `merge_model_settings` builds the base from `model_settings`, which copies only `compaction`, `cache`, `encoding`
and the provider settings' `rest`, then overlays the model's typed `ModelSettings` with `merge_overlay`
(`crates/catalog/src/provider.rs:303-320`). `ModelSettings.rest` (`crates/contract/src/generated/types.rs:28661-28666`)
holds every key outside `compaction`, `cache` and `encoding`, so a model's own `timeout`, `tls` or `transport` stays in
`rest` and survives into the resolved model's settings. Separately, `ModelService::transform` logs a failed
provider-snapshot `prepare` and continues with the transform (`crates/catalog/src/model.rs:174-188`).

Reason: the typed contract keeps unknown model settings for forward compatibility rather than dropping them, and the
model transform is registered during Location boot, where a failed prepare should not abort the boot. `catalog-fixture`
compares resolved provider and model bodies but never places provider-only keys inside a model's settings, so neither
the surviving-key case nor the failed prepare is exercised by a parity scenario.

Accepted: pending owner
