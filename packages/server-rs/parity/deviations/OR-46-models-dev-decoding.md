# OR-46: models.dev decoding accepts fewer date forms, keeps explicit nulls, and treats an empty aisdk string as present

TypeScript: `released` parses `release_date` with `Date.parse` and returns `0` only when the result is not finite
(`packages/core/src/models-dev.ts:327-330` at the pinned commit
`73a0eae88de47f10068a0f9dc7a743638fa3d830`). `Date.parse` accepts RFC 3339 as well as date-only (`YYYY-MM-DD`),
month-only (`YYYY-MM`), year-only (`YYYY`) and several non-ISO date forms. Models decode through Effect schemas:
`decodeModel` is `Schema.decodeUnknownResult(SourceModel)` and a failure is logged and the model dropped before
routing (`:141`, `:197-206`), and optional fields use `Schema.optional`, which accepts an absent key or `undefined`
but not an explicit `null` (`:99-126`), so `"status": null`, `"family": null` or `"limit": null` skips the model. In
`route`, eligibility and the package guard use JavaScript truthiness, so a chat API whose only SDK string is an empty
`sdk.aisdk` is not eligible (`:278-279`, `:283-293`).

Rust: `released` accepts RFC 3339 through `opencode_util::time::parse_rfc3339`, then `YYYY-MM-DD` through
`Date::parse`, then `YYYY-MM` and `YYYY` as the first day of that month or year, and returns `0.0` for any other
string (`crates/catalog/src/models_dev/normalize.rs:490-519`). `source.rs` decodes with serde into typed structs,
where `Option<T>` treats an explicit `null` exactly like an absent field
(`crates/catalog/src/models_dev/source.rs:5-29`, `:64-68`, `:143-171`, `:186-200`), so those catalogs keep the model
that TypeScript skips: `family` stays `None`, a null `limit` becomes absent with zero context and output, and a null
`status` becomes `active` (`normalize.rs:405-447`, `:460-467`).
`normalize.rs:241-250` decides eligibility with `Option::is_some`, so an empty `sdk.aisdk` string makes a chat API
eligible, and `normalize.rs:256-278` turns it into the non-empty package `aisdk:` (from
`crates/catalog/src/aisdk_native.rs:16-21`) where TypeScript finds no eligible API.

Reason: the port keeps a bounded date parser instead of a JavaScript-compatible one, and it decodes the catalog with
serde's typed optional handling. The pinned snapshot has no `null` values and no empty SDK strings, all of its
`limit.context` values are numbers, and its release dates are date-only or month-only, so none of these shapes
occurs in it. The models-dev corpus decodes that snapshot with both recommended flags and compares per-provider
canonical digests (`crates/catalog/tests/all/models_dev.rs:84-101`); the parity `catalog-fixture` scenario uses its
own small catalog from `models.json` instead of the pinned snapshot, so no parity scenario exercises these
differences.

Accepted: pending owner
