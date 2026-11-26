# OR-50: js-yaml 3 schema and parser features differ from yaml-rust2

TypeScript: `ConfigMarkdown.parse` reads frontmatter with gray-matter 4.0.3, whose default engine is js-yaml 3.15.2 `safeLoad` (`packages/core/src/config/markdown.ts:9-19`, pinned commit). Compared directly against the Rust parser, the following inputs produce different results. The TypeScript result is the value after JSON serialization, since `Frontmatter.data` holds plain JSON:

- `c: NULL` and `d: Null`: `null` for both (js-yaml 3 treats the case variants as null).
- `n: 012`: `10` (js-yaml 3 reads a leading zero as octal).
- `f: 0o17`: `"0o17"` (js-yaml 3 has no `0o` octal form, so it stays a string).
- `m: 1_000`: `1000` (js-yaml 3 accepts underscores in numbers).
- `s: 1:30`: `90` (js-yaml 3 accepts sexagesimal integers).
- a `<<` merge key, for example `base: {x: 1}` with `child: {<<: *base, y: 2}`: the merged mapping `{x: 1, y: 2}` (js-yaml 3 resolves merge keys).
- non-mapping frontmatter, `---\n- a\n- b\n---` or `---\njust text\n---`: `data` is the array or string itself, not an object.
- a complex flow key, `? [a, b]\n: c`: `{"a,b": "c"}`.
- a `---json` language header with a JSON body: gray-matter selects its JSON engine, so the body parses as JSON.

Rust: `crates/registry/src/markdown.rs::parse` builds a `serde_json::Map` with `yaml-rust2` 0.13, a YAML 1.2 parser with a JSON-compatible schema. The same inputs give:

- `c: NULL` and `d: Null`: the strings `"NULL"` and `"Null"`.
- `n: 012`: `12`.
- `f: 0o17`: `15` (YAML 1.2 octal).
- `m: 1_000`: the string `"1_000"`.
- `s: 1:30`: the string `"1:30"`.
- the merge key: a literal `"<<"` key holding the anchored mapping, and no merge into the parent object.
- non-mapping frontmatter: `None` (`parse_matter` requires a top-level hash at `markdown.rs:72-74`).
- the complex flow key: `None`, because the flow-mapping key is not a scalar and `yaml_key` rejects it.
- the `---json` header: `None` (`parse_matter` accepts only `yaml`/`yml` language headers at `markdown.rs:42-45`), and the `sanitize` retry also fails.

Reason: the registry crate needs a pure Rust frontmatter parser, and `yaml-rust2` is not a drop-in js-yaml 3 replacement. `YamlLoader` also discards scalar style, so the loader cannot tell `NULL` from `"NULL"`, `012` from `"012"` or `0o17` from `"0o17"`; matching js-yaml 3 for those would need a custom event receiver instead of `YamlLoader`, and merge keys and non-mapping documents would need extra code outside the parser. The corpus therefore does not add cases for these inputs (they would compare unequal); `crates/registry/tests/all/markdown.rs` pins the Rust behaviour above instead, and `crates/registry/tests/corpus/markdown.json` covers the cases both parsers agree on (the timestamp case is listed in `OR-50-frontmatter-yaml.md`).

Accepted: pending owner
