# OR-40: a legacy auth.json the TypeScript parser accepts is rejected whole

TypeScript: `packages/core/src/database/migration/20260805200742_import_legacy_credentials.ts:30` decodes the file
with `Schema.decodeUnknownOption(Schema.fromJsonString(Schema.Unknown))`, which is `JSON.parse`: an exponent such as
`1e400` becomes `Infinity` and an escape such as `"\ud800"` stays an unpaired surrogate, both accepted. The result is
only checked for being a non-array object (`:51-53`), then each entry is decoded against `LegacyValue` (`:29`,
`:57-58`) and an entry that fails is skipped with `continue` (`:58`), so one bad number or string loses that entry
alone and the rest of the file still imports.

Rust: `crates/storage/src/migration/hand.rs:100-107` parses the file with
`serde_json::from_str::<serde_json::Value>(&String::from_utf8_lossy(&bytes))` and, when that or the object match
fails, pushes `Skipped legacy credential file that is not a JSON object: <path>` into the report warnings and returns
`Ok(())` without importing anything. `serde_json` rejects `1e400` as a number out of range and `"\ud800"` as a lone
surrogate escape, so both inputs take that branch for the whole file, not just the affected entry.

Reason: `serde_json` is strict about numbers it cannot represent as `f64` and about unpaired surrogates, which a
UTF-8 `String` cannot hold, while the JavaScript parser is lenient and `Schema.Unknown` forwards whatever it produced.
An `auth.json` with such a value is hand-written or corrupt rather than produced by opencode, which writes valid
`JSON.stringify` output, so losing the whole file is reported as one warning the caller can surface. No check covers
this case: `a_malformed_legacy_credential_file_bootstraps_with_a_warning` in `crates/storage/tests/all/migration.rs`
exercises the same "not a JSON object" branch for `not json` only, and
`an_absent_legacy_credential_file_imports_nothing` covers an absent file.

Accepted: pending owner
