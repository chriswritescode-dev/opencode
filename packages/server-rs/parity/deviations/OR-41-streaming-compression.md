# OR-41: compressed stream bodies flush after every frame

TypeScript: `packages/server/src/process.ts:90` installs Effect's `HttpMiddleware.compression()`, which compresses response bodies through the platform `CompressionStream` and emits whatever that stream hands back per write. How it flushes a streaming body was not verified against the pinned Effect version.

Rust: `crates/http/src/compression.rs` wraps `response::Body::Stream` in `CompressedStream`, writes each upstream chunk into one persistent encoder and flushes it (gzip and deflate sync flush, brotli `CompressorWriter::flush`) before yielding the compressed bytes, then finishes the encoder with the matching end-of-stream operation.

Reason: `GET /api/event` and `GET /api/experimental/session/{sessionID}/log` must deliver each SSE frame as soon as it is produced, which needs a flush per frame; an encoder that only buffers holds frames until its internal block fills. The decompressed bytes are identical either way, and only compressed chunk boundaries and flush markers differ, which nothing compares between the two implementations: `crates/http/tests/all` checks decompressed order and that the first frame is readable before the stream ends.

Accepted: pending owner
