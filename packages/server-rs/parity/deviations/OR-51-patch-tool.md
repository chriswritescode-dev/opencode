# OR-51: `patch` targets one local environment and uses Rust whitespace

TypeScript: `packages/core/src/tool/plugin/patch.ts` resolves and locks every hunk through `FileAccess` and `FileMutation`, whose targets carry an `environment`, and applies writes, removals and renames through that environment's filesystem. Its recase check compares `environment.id` before deciding that a case-only move is a rename. The multi-environment cases live in `packages/core/test/tool-patch-target-environment.test.ts`.

Rust: `crates/tool/src/tools/patch.rs` takes a single `Access`, `Mutation`, `Authorizer` and `FormatHook` for the local environment, plus a `PatchFiles` seam for writes, removals, renames and entry kinds. Recasing therefore omits the same-environment comparison, and the multi-environment cases are not ported until the environment placement work (OR-60) lands.

Reason: the Rust fs layer has no machine/environment abstraction yet. Every case in `crates/tool/tests/corpus/patch.json` uses one local environment, and the ports of the forced write, move-removal and delete failures use a test `PatchFiles` instead of an environment override.

Two smaller differences:

- `parse` and `derive` use Rust `trim`, `trim_end` and `White_Space` where the TypeScript uses JavaScript whitespace definitions, and `PatchTools`' line matching uses the same normalisation table as `edit`.
- A preparation failure inside `Access::resolve` reports the `AccessError` text rather than the unwrapped `Environment.Failed` message, and a removed target that the test environment would have killed as a defect is an ordinary Rust failure. No corpus case covers either path.

Accepted: pending owner

```parity
```
