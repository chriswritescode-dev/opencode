# OR-42: a missing config file reference substitutes empty without a warning

TypeScript: `ConfigVariable.substitute` reads a `{file:path}` reference with `FSUtil.readFileString` and, on failure,
logs `config file reference is missing or unreadable` with the source, token, resolved path and cause, then
substitutes an empty string (`packages/core/src/config/variable.ts:68-76` at the pinned commit
`73a0eae88de47f10068a0f9dc7a743638fa3d830`).

Rust: `crates/config/src/variable.rs` reads the resolved path through a private UTF-8 helper and substitutes an empty
string on any read error. `substitute` returns only `String`, so it has no warning channel; the config crate has no
logger yet. The `file-missing` corpus case covers the empty substitution and the recorded reference.

Reason: the planned `substitute` API takes references and an environment lookup but returns only the substituted text.
The loader that calls it owns reporting, once the log layer from OR-42 exists.
