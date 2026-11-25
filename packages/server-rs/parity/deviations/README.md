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
- `parity` block: a fenced code block with one rule per line, required once the file is accepted (see [Matches](#matches)).

## Matches

The file stem is the deviation id that task text calls `<id>`. A non-empty `Accepted:` value is required: a value
starting with `pending` (any case) is not accepted, and the file then explains nothing. An accepted file must carry a
fenced `parity` block with one rule per line, or the run exits 2.

Each rule is `<scenario glob> <difference-path glob>`, matched with `Bun.Glob` semantics. The scenario name is its
path under `parity/scenarios` without `.scenario.ts`. The difference path uses the capture paths
(`steps/<i>/body/...`, `events/<i>/...`, `logs/<name>/...`, `model/<i>/...`,
`database/<file>/tables/<table>/...`, `files/<path>`), and a `ws` step's paths start with `steps/<i>/opened`,
`steps/<i>/frames` or `steps/<i>/close`.

```parity
info-auth steps/*/body/version
```

A scenario passes when every difference matches an accepted deviation; the harness prints the matched count and ids
on `PASS <scenario> (<n> explained by <ids>)`. After a full run with no scenario arguments, accepted deviations that
explained nothing print `warning: deviation <id> matched nothing`, and files without an accepted line print
`warning: deviation <id> is not accepted`.

A deviation that excepts a recorded cassette from the replay gate also has a `Cassettes:` paragraph listing the cassette names separated by commas. `crates/ai/tests/all/replay.rs` fails when a listed cassette is missing or replays cleanly.

## Current deviations

- `OR-38-lone-surrogates.md`: the strict round-trip corpus skips samples with unpaired UTF-16 surrogates
- `OR-38-request-bodies.md`: request bodies decode into the wire representation
- `OR-38-request-non-finite-numbers.md`: non-finite numbers in string slots are rejected
- `OR-39-not-implemented-operations.md`: a contract operation without a handler answers 501; implemented routes are listed in IMPLEMENTED
- `OR-39-persistent-pty-capability.md`: server info always reports the persistent PTY capability as unavailable
- `OR-39-port-owner.md`: a port-in-use message names no owning process
- `OR-39-wildcard-urls.md`: a wildcard bind reports only one loopback URL in server info
- `OR-39-remote-tunnel.md`: a `remote` service route prints a warning instead of starting a tunnel
- `OR-39-logging.md`: diagnostic lines go to stderr and no log file is written
- `OR-39-usage-errors.md`: malformed arguments print one fixed usage line and exit with code 1
- `OR-40-legacy-credential-json.md`: a legacy auth.json the TypeScript parser accepts is rejected whole
- `OR-40-migration-locking.md`: migrations take the write lock up front and import the Drizzle journal atomically
- `OR-40-migration-logging.md`: migrations log nothing and return a report instead
- `OR-41-query-decode-messages.md`: query decode failures carry the contract decoder's message text
- `OR-41-stream-failure-defect.md`: a log-read failure frame always names its defect `Error`
- `OR-42-canonical-failure.md`: a migrated legacy entry that fails re-decoding is dropped with an invalid diagnostic
- `OR-42-config-read-errors.md`: an unreadable or unresolvable config file is skipped instead of failing the load
- `OR-42-duration-precision.md`: durations beyond i128 nanoseconds or with 39 or more fractional digits are rejected
- `OR-42-file-home.md`: `{file:~/...}` expands from the configured home instead of `os.homedir()`
- `OR-42-macos-preferences-user.md`: managed preferences take the user name from `$USER` instead of the passwd entry
- `OR-42-observer-initial-apply.md`: the sources observer applies its initial load instead of returning it
- `OR-42-watcher-ignore.md`: config watching ignores directories per event and uses one native watcher per target
- `OR-43-heap-dump.md`: a heap dump writes a JSON memory report instead of a V8 heap snapshot
- `OR-43-project-query-decode-messages.md`: a project.list query decode failure carries the contract decoder's message text
- `OR-43-storage-failure-messages.md`: an unexpected 500 always carries the generic message, never a readable storage failure
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
- `OR-45-google-adc.md`: Google ADC supports authorized users, service accounts and the metadata server only
- `OR-45-google-interactions-continuation.md`: Google Interactions continuation does not resume a continued or dropped stream
- `OR-45-executor-stage-order.md`: credential resolution runs before request compilation, so a missing credential masks a request error
- `OR-45-media-retry.md`: media read retries use a deterministic schedule and honour retry-after for every reason they retry
- `OR-45-generation-progress.md`: queued media streams emit only terminal events, with no per-poll progress observation
- `OR-45-media-route-internals.md`: queued media result decoders take an injected clock, and result assets carry download headers but no middleware
- `OR-45-request-decoding.md`: modality request fields decode leniently
- `OR-45-response-decoding.md`: media, realtime and decision responses decode leniently
- `OR-45-multipart-numbers.md`: repeated-array numbers in multipart bodies use Rust formatting
- `OR-45-decision-parity.md`: OpenAI decisions keeps unknown prompt-cache keys and names its inline route by protocol
- `OR-45-provider-construction.md`: provider configuration failures differ in timing and error class
- `OR-45-xai-responses-websocket.md`: xAI streams over HTTP only and has no chat route
- `OR-45-extraction.md`: extraction validates requests less strictly and forwards provider options
- `OR-45-corpus-normalisation.md`: the corpus normalises transport noise and replay skips some byte comparisons
- `OR-49-registry-events.md`: plugin and catalog registries publish no updated events
- `OR-49-prompt-attachments.md`: prompt file attachments and skills are rejected instead of materialized
- `OR-49-title-generation.md`: an empty title does not regenerate the session title
- `OR-61-sidecar-environment.md`: plugins run in a sidecar with an allowlisted environment instead of inside the server
