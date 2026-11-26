# OR-48: a malformed Console config value is dropped instead of rejecting the whole config

TypeScript: `fetchConfig` decodes the Console document strictly with
`Schema.decodeUnknownEffect(ConfigIntegration.Remote)` (`packages/core/src/plugin/provider/opencode.ts:615`). One
malformed allow-listed value, such as a provider whose settings have the wrong type, fails the load, so
`SharedCache` keeps the last good config and the load reports the failure.

Rust: `crates/catalog/src/provider_plugin/opencode/remote.rs:130` passes the body to
`opencode_config::integration::remote`, which normalises only `providers`, `mcp`, `policies` and `websearch`
(`crates/config/src/integration.rs:10-34`). `normalize` drops a malformed individual value with a diagnostic instead of
rejecting the document, and only a document that normalisation rejects fails the load. A malformed policy statement
whose action or effect is unknown is dropped first (`:45-52`), and a malformed kept statement still fails.

Reason: the config crate normalises every config source through one path, so the Console document keeps the same
tolerance as the rest of the configuration instead of a second strict Schema decoder. The ported tests cover unknown
future values and malformed statements (`crates/catalog/tests/all/provider_plugin/opencode.rs`); no parity scenario
feeds a malformed kept provider value.

Accepted: pending owner
