# Deviations

A difference from the TypeScript server at the commit in `../../upstream-pin` is allowed only as a file in this
directory that the owner has accepted. A difference without a file is a failure.

## Format

Files are named `<area>-<slug>.md`, where the area is the task that owns the difference (`OR-52`) or the port area
(`session`, `pty`) and the slug is short and kebab-case. The first line is the heading
`# <area>: <one-line difference>`.

Each file has these paragraphs, in order:

- `TypeScript:` what the TypeScript server does, with `path:line` at the pinned commit.
- `Rust:` what this package does instead, with the crate and function or file.
- `Reason:` why the difference exists, and which parity scenario or check covers it.
- `Accepted:` the owner's name and the date the deviation was accepted.

## Current deviations

- `OR-38-lone-surrogates.md`: the strict round-trip corpus skips samples with unpaired UTF-16 surrogates
- `OR-38-request-bodies.md`: request bodies decode into the wire representation
- `OR-38-request-non-finite-numbers.md`: non-finite numbers in string slots are rejected
- `OR-39-not-implemented-operations.md`: only three operations are implemented and the rest answer 501
- `OR-39-persistent-pty-capability.md`: server info always reports the persistent PTY capability as unavailable
- `OR-39-port-owner.md`: a port-in-use message names no owning process
- `OR-39-wildcard-urls.md`: a wildcard bind reports only one loopback URL in server info
- `OR-39-remote-tunnel.md`: a `remote` service route prints a warning instead of starting a tunnel
- `OR-39-logging.md`: diagnostic lines go to stderr and no log file is written
- `OR-39-usage-errors.md`: malformed arguments print one fixed usage line and exit with code 1
- `OR-41-streaming-compression.md`: compressed stream bodies flush after every frame
- `OR-44-aws-credentials.md`: Bedrock credentials come only from route configuration, not the AWS default chain
- `OR-44-bedrock-output.md`: Bedrock frames, cache warnings and SigV4 output carry different wire details
- `OR-44-client-tls.md`: per-request client certificates are not presented
- `OR-44-compaction-execution.md`: compaction and checkpoint requests are built but never posted or decoded
- `OR-44-gemini-continuation.md`: Gemini continuation does not resume dropped connections or reprepare headers
- `OR-44-lenient-decoding.md`: provider options and stream frames decode leniently and errors name the protocol
- `OR-44-lone-surrogates.md`: request strings cannot carry lone UTF-16 surrogates
- `OR-44-request-encoding.md`: zstd and gzip request bodies are rejected instead of compressed
- `OR-44-response-fold.md`: an unfinished attempt folds into a response without a finish reason
- `OR-44-retry-after.md`: HTTP-date retry-after accepts a narrower format set and uses the injected clock
- `OR-44-stream-error-bodies.md`: mid-stream error bodies are re-encoded instead of the original frame text
- `OR-44-transport-codes.md`: HTTP transport error codes are Rust I/O kinds instead of Node errno names
- `OR-44-utf16-truncation.md`: id and name limits count Unicode scalars instead of UTF-16 code units
- `OR-44-websocket-channel.md`: WebSocket failures omit readyState and OS codes, and the HTTP fallback is unused
