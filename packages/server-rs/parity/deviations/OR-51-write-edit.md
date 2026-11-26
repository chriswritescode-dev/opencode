# OR-51: `write` and `edit` differ at two Unicode and path-kind edges

TypeScript: `packages/core/src/tool/plugin/edit.ts:188-239` truncates each hint line at 200 UTF-16 code units and compares hint lines with a whitespace-collapsing regular expression. `packages/core/src/tool/plugin/edit.ts:301-313` learns that the target is a directory from the environment's `Environment.WrongKind` error.

Rust: `crates/tool/src/tools/edit.rs` truncates hint lines at 200 Unicode scalar values and collapses whitespace with `split_whitespace`. It detects a directory with `tokio::fs::metadata` before reading, and maps every other read failure to `Unable to edit <path>`.

Reason: the corpus cases in `crates/tool/tests/corpus/edit.json` cover the closest-lines hint, the already-applied hint, the read-denied suppression, the missing-file and directory messages and the mixed line-ending bytes, all with characters inside the BMP. An astral character in a hint line would count as two TypeScript units and one Rust scalar; a non-directory wrong kind (a device or socket) reports `Unable to edit` instead of the environment's message.

Accepted: pending owner

```parity
```
