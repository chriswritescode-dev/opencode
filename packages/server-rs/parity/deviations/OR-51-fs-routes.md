# OR-51: fs routes parse HTTP dates only, take any write content type, and probe workspace directories at boot

TypeScript: `packages/server/src/handlers/fs.ts:155-160` compares validators with `Date.parse`, which also accepts non-HTTP date strings such as ISO 8601. `packages/protocol/src/groups/fs.ts:83-86` declares the write payload as `Uint8Array`, so the Effect HTTP layer rejects a request whose content type is JSON with 415. `packages/core/src/filesystem.ts:120-132` skips the realpath and readability probe when the location has a `workspaceID`, because a workspace directory exists only inside the workspace.

Rust: `crates/http/src/handlers/fs.rs` parses `If-Modified-Since` and `If-Range` with `httpdate`, so only RFC 1123, RFC 850 and asctime dates match. `experimental.fs.write` writes the raw request body whatever its content type. `crates/http/src/services.rs` builds `LocationFileSystem::new` for every Location, which canonicalizes and probes the directory and fails boot when it is missing, so a workspace-backed Location whose directory is not present on the server host does not boot.

Reason: `httpdate` is already in the lock file and formats `Last-Modified` exactly as the TypeScript server does, while accepting the wider set `Date.parse` understands would need a second date parser. A content-type gate for writes has no parity case. Workspace placement is reserved and not wired in this package yet. `parity/scenarios/fs-routes.scenario.ts` covers ranges, conditional requests, list, find, a write with `application/octet-stream`, and watch/unwatch; none of the narrower behaviours is exercised.

Accepted: pending owner

```parity
```
