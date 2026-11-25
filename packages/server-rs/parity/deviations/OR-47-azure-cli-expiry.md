# OR-47: an Azure CLI token without `expires_on` reads `expiresOn` as UTC

TypeScript: `packages/core/src/plugin/provider/azure.ts:100` uses `expires_on * 1000` when the `az account get-access-token` output has it, and otherwise `Date.parse(expiresOn)`. The CLI writes `expiresOn` as a local time without an offset (for example `2024-01-01 12:00:00.000000`), which `Date.parse` reads in the process time zone.

Rust: `crates/catalog/src/provider_plugin/azure/discovery.rs` `parse_expires_on` reads that value as UTC. When the CLI omits `expires_on`, a cached Entra token is kept up to the UTC offset past its real expiry east of UTC, and is refreshed on every use west of UTC.

Reason: the catalog crate has no time-zone dependency and the project adds no new third-party dependency for this. Current Azure CLI versions return `expires_on`, so only older CLI versions reach this path.

Accepted: pending owner
