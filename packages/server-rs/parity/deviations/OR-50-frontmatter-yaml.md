# OR-50: YAML timestamp frontmatter stays a string instead of a `Date`

TypeScript: `ConfigMarkdown.parse` reads frontmatter with gray-matter 4.0.3, whose default engine is js-yaml 3.15.2 `safeLoad` (`packages/core/src/config/markdown.ts:9-19`). js-yaml's default safe schema includes the timestamp type (`node_modules/js-yaml/lib/js-yaml/schema/default_safe.js`, `type/timestamp.js`), so an unquoted ISO date or timestamp becomes a JavaScript `Date`. For the omitted case `date` (`created: 2020-01-02`, `stamp: 2020-01-02T03:04:05Z`) the data values are `Date` instances that serialize as `2020-01-02T00:00:00.000Z` and `2020-01-02T03:04:05.000Z`.

Rust: `crates/registry/src/markdown.rs` reads YAML with `yaml-rust2` 0.13, a YAML 1.2 parser without the js-yaml timestamp type, so the same scalars become the strings `"2020-01-02"` and `"2020-01-02T03:04:05Z"`.

Reason: the registry crate needs a pure Rust YAML frontmatter parser and js-yaml's timestamp extension is not part of YAML 1.2. The corpus omits this case rather than comparing unequal values: `crates/registry/tests/corpus/markdown.json` does not contain `date`, and the input is listed here so the omission stays visible. No other case from the ported markdown suite is omitted.

Accepted: pending owner
