# OR-47: Azure deployment discovery stops when a page link repeats

TypeScript: `managementDeployments` follows `nextLink` through `Stream.paginate` with no visited set or page cap (`packages/core/src/plugin/provider/azure.ts:207`, `:218`), so a response whose `nextLink` points back at an already fetched page keeps discovery running while it re-requests that page.

Rust: `crates/catalog/src/provider_plugin/azure/discovery.rs` (`management_pages`) tracks the page URLs it has requested and fails discovery with "Azure repeated a deployment page" when a `nextLink` names one again. The existing origin check still applies.

Reason: discovery runs as a background job whose `busy` flag suppresses the next interval tick, so a repeated link would keep the job running and the deployment list stale forever. Failure keeps the last complete list, clears the cached resource ID and lets the resource's own deployment list stand in, as any other page failure does. A legitimate paginated list visits each page URL once and is unaffected.

Accepted: pending owner
