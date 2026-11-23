# OR-39: a wildcard bind reports only one loopback URL in server info

TypeScript: `ServerInfo.connectionURLs` (`packages/server/src/server-info.ts:18-39`) maps a `0.0.0.0` or `::` bind
to the matching loopback address and adds every non-internal interface address of that family, so `/api/info` lists
every reachable URL.

Rust: `crates/server/src/serve.rs:137-146` maps an unspecified bind address to loopback, and `crates/http/src/app.rs`
`server_info` reports that single URL, so a wildcard bind reports only `http://127.0.0.1:<port>` (or
`http://[::1]:<port>`). `serve --service` registers `http://<bound address>:<port>` in
`crates/server/src/service_mode.rs`, so discovery uses the wildcard address while `/api/info` reports loopback.

Reason: the interface scan is not ported yet and belongs with the paths/configuration task. The service still starts
and registers, so only the reported URL list narrows. No check yet.

Accepted: pending owner
