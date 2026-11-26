# OR-51: missing-path recovery folds Unicode differently from the TypeScript server

TypeScript: `packages/core/src/tool/plugin/read.ts:320-337` compares look-alike filenames and folder entries after `String.prototype.normalize("NFC")`, lowercases with `toLowerCase()`, and orders equally ranked suggestions with `localeCompare`.

Rust: `crates/tool/src/tools/read.rs` `canonical` replaces the non-breaking spaces and curly quotes without applying NFC, `similar` lowercases with Rust's `to_lowercase()`, and the last sort key is byte order rather than `localeCompare`.

Reason: the workspace has no Unicode normalization or ICU dependency, and adding one only for the suggestion ranking is not justified. The corpus cases in `crates/tool/tests/corpus/read.json` cover the non-breaking-space recovery, the ambiguous-file skip, the exact/other-extension/containing/typo tiers and the three-suggestion cap; no parity scenario exercises a decomposed accent or a locale-specific ordering.

Accepted: pending owner

```parity
```
