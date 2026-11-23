# OR-44: HTTP-date retry-after accepts fewer formats and uses the injected clock

TypeScript: `packages/ai/src/route/executor.ts:57-70` parses `retry-after-ms` and `retry-after` seconds with `Number`, then falls back to `Date.parse(value)` and subtracts `Date.now()`. `Date.parse` accepts the RFC 2822 date grammar and ISO-8601, so spellings beyond the HTTP-date forms still produce a delay (pinned commit `73a0eae88de47f10068a0f9dc7a743638fa3d830`).

Rust: `crates/ai/src/provider_error.rs` `retry_after_ms` handles `retry-after-ms` and numeric seconds the same way, then calls `parse_http_date`. That accepts ISO-8601 (`YYYY-MM-DD` with a `T` or space, optional fraction, `Z` or `±HH:MM`) through `parse_iso_date`, and the IMF-fixdate shape `[Www, ]DD Mon YYYY HH:MM:SS GMT`. Its RFC 850 and asctime branches do not match those formats' actual part counts, so `Sunday, 06-Nov-94 08:49:37 GMT` and `Sun Nov  6 08:49:37 1994` fall through to no value. Its `now_ms` comes from the `Clock` passed to `http_failure`, not from the process, so results are deterministic.

Reason: replay needs a fixed clock, and the corpus in `script/llm/errors.ts:496-500` covers `retry-after-ms`, numeric seconds and an IMF-fixdate produced by `toUTCString()`. A `retry-after` value outside the accepted set, including the RFC 850 and asctime forms HTTP permits, yields no `retry_after_ms` here where TypeScript's `Date.parse` would parse it.

Accepted: pending owner
