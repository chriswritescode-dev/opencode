# OR-44: HTTP transport error codes are Rust I/O kinds instead of Node errno names

TypeScript: `packages/ai/src/route/executor-service.ts:144-170` sets `Transport.code` from the native Node error (`ECONNREFUSED`, `ENOTFOUND`, `EAI_AGAIN`, ...) and prefixes the message with it, falling back to `Timeout` or the Effect reason tag. `packages/core/src/model-retry.ts:42,49,107,118` matches these names to pick the unreachable-host budget and the not-sent retry (pinned commit `73a0eae88de47f10068a0f9dc7a743638fa3d830`).

Rust: `crates/ai/src/route/executor.rs` `native_code` returns the `Debug` name of the first `std::io::ErrorKind` in the error's source chain (`ConnectionRefused`, `TimedOut`, ...), and the transport error falls back to `Transport` when none is found, which includes most DNS failures reported by reqwest.

Reason: the Node errno vocabulary is a property of the Node runtime, not of the provider wire format, and no cassette or corpus row records a transport failure. The session runner (OR-49) owns the retry policy that reads these codes and must map them when it ports `model-retry.ts`.

Accepted: pending owner
