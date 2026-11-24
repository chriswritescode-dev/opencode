# OR-43: an unexpected 500 always carries the generic message, never a readable storage failure

TypeScript: `packages/server/src/middleware/defect-response.ts:13-35` answers an unhandled failure with `500` and an
`UnknownError` holding a fresh `err_<8 hex>` ref, logging the cause with that ref. Its `message` is
`StorageFailure.message(cause)` (`packages/core/src/database/storage-failure.ts:11-27`) when the cause is a SQLite
storage failure, such as `Storage is full` or `Storage is in use by another process`, and otherwise
`Server failed to handle the request. Check server logs for details.`

Rust: `crates/http/src/handlers/defect.rs::response` answers the `project.list` and `project.update` storage failures,
a failed or panicked Location boot (`crates/http/src/services.rs::location_error`) and a failed
`experimental.config.update` with the same status, tag, ref and log line, but always with the generic message.

Reason: the storage crate does not yet classify SQLite result codes into the TypeScript storage messages, so there is
no readable message to reuse. Clients that branch on the status, tag or ref see no change; only the human-readable
`message` differs for storage failures. No parity scenario can provoke a storage failure, so none covers this.

Accepted: pending owner
