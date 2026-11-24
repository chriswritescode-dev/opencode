# OR-43: a heap dump writes a JSON memory report instead of a V8 heap snapshot

TypeScript: `packages/server/src/handlers/debug.ts:17` delegates to `HeapSnapshot.write`, and
`packages/util/src/heap-snapshot.ts:15` writes a V8 `.heapsnapshot` of the process, then returns the `path`, `pid`
and `process.memoryUsage()`. The file is named `heap-<pid>-<ISO time>.heapsnapshot`, with the ISO timestamp's `:`
and `.` removed.

Rust: `crates/http/src/handlers/debug.rs` writes `heap-<pid>-<compact ISO time>.json` containing the `pid`, the
resident set size derived from `/proc/self/statm` and the page size, and the fields of `/proc/self/status`, or zeros
and an empty object where those files are unavailable. The response keeps the same `path`, `pid` and `memory` shape,
but `rss` is the only populated memory field: `heapTotal`, `heapUsed`, `external` and `arrayBuffers` are always `0`,
because the Rust server has no JavaScript heap to measure.

Reason: Rust has no V8 runtime or JavaScript heap to serialize. The endpoint keeps the contract and the owner-only
file guarantee so a client can still collect a process memory sample for diagnosis, but the file cannot be loaded as
a Chrome DevTools heap snapshot.

Accepted: pending owner

```parity
debug* steps/*/body/memory/*
debug* steps/*/body/path
```
