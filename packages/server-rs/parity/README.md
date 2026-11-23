# Server parity harness

Runs a scenario against the TypeScript reference server and a candidate server, normalizes both captures, and reports the first point where they differ.

## Running

- `bun run parity --candidate <ts|path> [scenario...]` runs scenarios against the candidate. `ts` builds the command for the TypeScript server in this repository, and any other value is treated as the path to an executable that accepts `serve --stdio --port 0`.
- `bun run parity:test` runs the harness unit and subprocess tests.
- `--reference <ts|path>` selects the reference side and defaults to `ts`.
- `--candidate <ts|path>` is required.
- `--repeat <n>` repeats every scenario `n` times and defaults to `1`; values must be positive integers. Repetition stops at the first repetition that reports a difference.
- Remaining positional arguments are scenario files. With none, every `parity/scenarios/*.scenario.ts` file runs in sorted order. A scenario name is its path relative to `parity/scenarios` without `.scenario.ts`, falling back to the basename for files outside that directory.
- Each run writes `parity/out/<scenario>/reference.json`, `candidate.json` and `differences.json`. Each repetition overwrites the previous files.
- At startup the harness reads `packages/server-rs/upstream-pin`. When that commit exists in the clone and the TypeScript reference differs from it under the pinned source directories, a warning line names the number of changed files.

## Steps

Each scenario runs its `steps` in order. `http` steps add an entry to `steps`; `waitEvent` and `files` steps do not.

- `http`: `method` and `path` are required. `query` values are strings or arrays, and array values repeat the key. `headers` are sent as written and `body` is JSON. `auth` is `basic` (default), `query` or `none`. `location` defaults to `true` and sends `x-opencode-directory: <work>`. `as` saves the parsed response body for later placeholders.
- `waitEvent`: waits for an event of `type`. `match` is an optional deep-partial match against the event's `data` after templating: every object key must match and arrays compare as a prefix. `timeout` is in milliseconds and defaults to `30000`. The next wait starts after the previously matched event.
- `files`: writes a `{ path: contents }` map below `work` mid-scenario, unlike the scenario-level `files` written before the server starts.
- Templating: `{{work}}`, `{{root}}` and `{{<as>.<dotted.path>}}` are replaced in paths, query values, header values and the string leaves of request bodies and `match` objects. An unresolved placeholder fails the scenario.

## Sandbox and environment

Each side pair gets a fresh random root created with `mkdtemp` under `PARITY_SANDBOX_DIR` (default `/tmp/opencode-parity`). The root contains fixed `home`, `home/.config`, `home/.local/share`, `home/.local/state`, `home/.cache`, `tmp` and `work` directories. The reference runs first, then `resetSandbox` removes the root and recreates those directories before the candidate runs, so neither side sees the other's state. Scenario `files` are written below `work`, the server runs with `work` as its working directory, and requests default to the `work` directory.

Every variable is set for both sides:

- `PATH`: inherited from the harness so `bun`, `git` and other tools resolve normally.
- `LANG`: `C.UTF-8` keeps output and file names UTF-8 regardless of the host locale.
- `HOME`: the sandbox home so no side reads or writes the real home directory.
- `OPENCODE_TEST_HOME`: the same sandbox home, so test-mode lookups stay isolated too.
- `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME`, `XDG_CACHE_HOME`: all below the sandbox home, keeping configuration, the database, state and caches inside the root.
- `TMPDIR`: `root/tmp`, so temporary files stay inside the root and normalize away.
- `OPENCODE_PASSWORD`: `parity`, a fixed credential both sides accept through Basic auth and `auth_token`.
- `OPENCODE_DISABLE_AUTOUPDATE`: `1` prevents an upgrade check from contacting the network.
- `OPENCODE_DISABLE_MODELS_FETCH`: `1` prevents the model catalog from being downloaded.
- `OPENCODE_MODELS_PATH`: `root/models.json`, an empty catalog file used instead of a fetch.
- `OPENCODE_DISABLE_FILEWATCHER`: `1` avoids filesystem watchers in a throwaway directory.
- `OPENCODE_DISABLE_CHANNEL_DB`: `1` avoids a channel-specific database path.
- `OPENCODE_FLAGS`: `-full_codemode` keeps optional codemode behavior out of the comparison.
- `OPENCODE_CONFIG_CONTENT`: inline JSON that disables updates and the title agent, merged with the scenario's `config`.
- `BUN_RUNTIME_TRANSPILER_CACHE_PATH`: a shared cache next to the sandbox root, so each run does not transpile from scratch.
- `PARITY_SANDBOX_DIR`: parent directory for sandbox roots.
- `PARITY_START_TIMEOUT`: milliseconds to wait for a server's first stdout line, default `60000`.
- `PARITY_CANDIDATE_ENV`: JSON object of extra string variables applied only to the candidate side.

## Normalisation rules

Rules apply to every string, number and object key in a capture.

| id | Replaces | Why |
| --- | --- | --- |
| `root` | The sandbox root and its URL-encoded form with `<root>`. | Both sides run in different random roots, and absolute paths appear in URLs and payloads. |
| `port` | `<host>:<port>` for loopback `127.0.0.1:<port>` and `localhost:<port>`. | Each server binds an ephemeral port that differs between sides. |
| `id` | Prefixed ids with `<prefix:n>` using first-seen numbering per capture. | Freshly generated session and message ids differ but should play the same role. |
| `slug` | Session slugs with `<slug:n>`, numbered per first appearance and kept separate from ids. | The server picks a random human-readable slug per session. |
| `time` | Millisecond numbers near the capture window and ISO-8601 timestamps with `<time>`. | Wall-clock values differ between the reference and candidate runs. |
| `process` | `pid` and `hostname` values with `<pid>` and `<hostname>`. | Process ids differ per side and hostnames are machine specific. |
| `base64` | Base64 strings that decode to a JSON object or array are decoded, normalised recursively, and re-encoded. | Session-list cursors encode session ids and timestamps. |

Strings that look like JSON objects or arrays are parsed, normalized recursively, and serialized back, so JSON nested inside a string field is compared on its structure rather than its exact ids and times. Object keys are normalised too, so a `logs` map keyed by a raw session id becomes the same `<ses:n>` placeholder as its value.

## Captures

A capture is `{ steps, events, logs, model, database, files }`.

- `steps`: one entry per `http` step, `{ status, headers, body }`. `status` is the numeric response status. `headers` contains lower-cased header names sorted alphabetically, dropping `date`, `connection`, `keep-alive`, `content-length`, `transfer-encoding`, `etag` and `last-modified`. `body` is parsed as JSON when the response content type contains `json` and kept as text otherwise, so a 401 error body is captured but never asserted by the scenario itself.
- `events`: every `/api/event` frame collected in arrival order. The stream opens before the first step and closes after the last, so it starts with `server.connected`.
- `logs`: for each session created in the run, the parsed frames of `GET /api/experimental/session/<id>/log`, keyed by the raw session id.
- `model`: outbound model requests, currently always `[]`.
- `database`: one entry per SQLite file under the sandbox, keyed by its relative path, captured after the server has exited. Each entry has `schema` (`type`, `name`, `tbl_name`, `sql`, sorted by type then name) and `tables` (one array of row objects per table). Blob values are `hex:<lowercase hex>`, and table rows are sorted by their serialised JSON after normalisation, so insertion order of random ids does not matter.
- `files`: every other file under the sandbox as relative path to contents, captured after the server has exited. Valid UTF-8 files store their text and other files store `sha256:<hex>`. Excluded are SQLite files and their `-wal`, `-shm` and `-journal` siblings (captured as `database` instead), `models.json` (written by the harness), and `home/.local/share/opencode/log/**` (log lines carry wall-clock times, run ids and process ids).

Each run writes three files below `parity/out/<scenario>/`: `reference.json` and `candidate.json` hold the normalized captures, and `differences.json` holds the compared paths with their reference and candidate values.

## Exit codes

- `0`: every scenario matched.
- `1`: at least one scenario reported a difference.
- `2`: a scenario, start or configuration error, such as a server that did not start or an invalid flag value.
