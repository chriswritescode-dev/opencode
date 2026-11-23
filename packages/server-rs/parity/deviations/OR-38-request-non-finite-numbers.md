# OR-38: non-finite numbers in string slots are rejected

TypeScript: `Schema.NumberFromString` decodes with the global `Number()`, so `NaN`, `Infinity` and `-Infinity`
survive unless the field adds a finite check. Query fields built without one, such as `experimental.session.stats`'
`from` and `to` (`packages/protocol/src/groups/experimental.ts`), accept `?from=abc` and decode it as `NaN`.

Rust: `crates/contract/src/request.rs` `number_value` rejects a value that `opencode_api_types::js::parse_number`
cannot turn into a finite `serde_json::Number`, so `?from=abc` fails with "expected a number".

Reason: `serde_json` has no representation for `NaN` or the infinities, and the typed JSON expected by the contract
corpus cannot carry them either. The corpus generator drops such samples (they do not render), and
`crates/contract/tests/all/requests.rs` covers rejection of every invalid sample it does record.

Accepted: pending owner
