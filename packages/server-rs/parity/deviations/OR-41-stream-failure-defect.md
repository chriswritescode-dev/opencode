# OR-41: a log-read failure frame always names its defect "Error"

TypeScript: `GET /api/experimental/session/{sessionID}/log` streams `log` from `packages/core/src/bus.ts:829-872`,
which reads stored events through `readAfter` (`packages/core/src/bus.ts:765-807`) and decodes every row with
`decodeSerializedEvent` and `decodeStoredData` (`packages/core/src/bus.ts:81-94`, called at
`packages/core/src/bus.ts:796`). A malformed payload therefore throws inside the stream: a JSON `SyntaxError` when
the stored `data` does not parse, or a schema `ParseError` when `Schema.decodeUnknownSync` rejects it. The HTTP layer
serialises that thrown defect itself as the `effect/http-api/stream/failure` cause, so the frame keeps the defect's
own `name` and `message`.

Rust: `crates/http/src/handlers/session_log.rs` `log` maps a failed `state.bus.log` item through `cause`, which
always writes `[{"_tag":"Die","defect":{"name":"Error","message":"<BusError text>"}}]`. `name` is the literal
`"Error"` for every failure, and `message` is `opencode_bus::BusError`'s display text: the stored message for
`InvalidDurableEvent`, or the transparent `StorageError`/`rusqlite::Error` text (`crates/bus/src/error.rs:5-12`).

Reason: the Rust bus folds decode failures into `BusError::InvalidDurableEvent` with a message and does not retain
the underlying error type, so it has no thrown `SyntaxError` or `ParseError` name to report. Status, content type,
the `effect/http-api/stream/failure` event name and the `Die` cause shape all match; only the defect `name` and the
`message` wording differ. The corpus `failure` record in `crates/session/tests/corpus/events.json` covers only a
synthetic `new Error("log reader failed")`, whose name is already `"Error"`, so
`crates/http/tests/all/session_log.rs::an_undecodable_row_ends_with_the_recorded_failure_frame` compares equal and
no check covers a real decode failure.

Accepted: pending owner
