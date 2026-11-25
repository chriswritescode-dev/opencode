# Server parity harness

Runs a scenario against the TypeScript reference server and a candidate server, normalizes both captures, and reports the first point where they differ.

## Running

- `bun run parity --candidate <ts|path> [scenario...]` runs scenarios against the candidate. `ts` builds the command for the TypeScript server in this repository, and any other value is treated as the path to an executable that accepts `serve --stdio --port 0`.
- `bun run parity:test` runs the harness unit and subprocess tests.
- `--reference <ts|path>` selects the reference side and defaults to `ts`.
- `--candidate <ts|path>` is required.
- `--repeat <n>` repeats every scenario `n` times and defaults to `1`; values must be positive integers. Repetition stops at the first repetition that reports a difference.
- `--deviations <dir>` selects the deviation ledger and defaults to `packages/server-rs/parity/deviations`; tests use it to supply a temporary ledger.
- `--only <parts>` restricts the comparison to a comma list drawn from `steps,events,database,files,logs,model` and defaults to all six; see [Comparison modes](#comparison-modes).
- `--tables <names>` is a comma list of table names that restricts the database comparison per database file; it requires `database` in `--only`.
- `--allow-pending` loads deviations whose `Accepted:` line starts with `pending` and matches them like accepted ones, so a candidate can be checked before the owner accepts the deviation; see [Deviations](#deviations).
- Remaining positional arguments are scenario files. With none, every `parity/scenarios/*.scenario.ts` file runs in sorted order. A scenario name is its path relative to `parity/scenarios` without `.scenario.ts`, falling back to the basename for files outside that directory.
- Each run writes `parity/out/<scenario>/reference.json`, `candidate.json` and `differences.json`, and, once at least one scenario has been captured, `parity/out/coverage.md` and `parity/out/coverage.json`. Each repetition overwrites the previous files.
- At startup the harness reads `packages/server-rs/upstream-pin`. When that commit exists in the clone and the TypeScript reference differs from it under the pinned source directories, a warning line names the number of changed files.

## Scenarios

`parity/scenarios` currently holds:

- `agent-model-list`: `GET /api/agent` and `GET /api/model`.
- `config-routes`: `GET /api/config`, `GET /api/config/shell`, `PATCH /api/experimental/config`, then a re-read.
- `debug-routes`: `POST /api/debug/heap-dump`.
- `file-read`: `GET /api/fs/read` for an existing and a missing file, and `GET /api/fs/list`.
- `info-auth`: `GET /api/info` with basic, query and no credentials.
- `location`: a location reload and a subdirectory Location load, each waiting for `plugin.updated`.
- `location-routes`: the location, debug-location list, evict and reload routes, and directory-scoped loads including a missing directory.
- `project-routes`: project list and update, and archived-query validation.
- `prompt-text`: a text prompt through the Anthropic route.
- `prompt-text-only`: a text prompt through the Anthropic route with every tool denied, so the model request has an empty tool list.
- `prompt-tool`: a scripted `read` tool call.
- `session-create-list`: session create with and without a title, the `session.created` events, list and get.

## Steps

Each scenario runs its `steps` in order. `http` and `ws` steps add an entry to `steps`; `waitEvent` and `files` steps do not.

- `http`: `method` and `path` are required. `query` values are strings or arrays, and array values repeat the key. `headers` are sent as written and `body` is JSON. `auth` is `basic` (default), `query` or `none`. `location` defaults to `true` and sends `x-opencode-directory: <work>`. `as` saves the parsed response body for later placeholders.
- `waitEvent`: waits for an event of `type`. `match` is an optional deep-partial match against the event's `data` after templating: every object key must match and arrays compare as a prefix. `timeout` is in milliseconds and defaults to `30000`. The next wait starts after the previously matched event.
- `files`: writes a `{ path: contents }` map below `work` mid-scenario, unlike the scenario-level `files` written before the server starts.
- `ws`: `path` is required, and the URL is the server base with the HTTP scheme replaced by `ws` (`wss` for `https`);
  `query` behaves as it does for `http`, with string or array values and arrays repeating the key. `auth` is `basic`
  (default), `query` or `none`, as for `http`. After the socket opens, each string in `send` is sent in order. Text
  frames are collected until `receive` frames have arrived or the socket closes, then the harness closes the socket
  with code 1000. `timeout` is in milliseconds and defaults to `10000`. `as` saves the capture for later placeholders.
  Templating (`{{work}}`, `{{root}}`, `{{<as>.<path>}}`) applies to `path`, query values and `send`.
- Templating: `{{work}}`, `{{root}}` and `{{<as>.<dotted.path>}}` are replaced in paths, query values, header values and the string leaves of request bodies and `match` objects. An unresolved placeholder fails the scenario.

## Adding a scenario

A scenario is a `.scenario.ts` file under `parity/scenarios` that default-exports `defineScenario({ ... })`. Because the output directory and the deviation rules key off the file path, name it `<name>.scenario.ts`; the scenario name is that path below `parity/scenarios` without the suffix. `files`, `config`, `env` and `model` are optional and `steps` is required. Top-level `files` are written under `work` before the server starts, `config` deep-merges over the sandbox base config, and `env` adds string variables to both sides.

```ts
import { defineScenario } from "../lib/scenario"

export default defineScenario({
  files: { "hello.txt": "hello parity\n" },
  config: { model: "anthropic-test/test-model" },
  model: [
    { tool: { id: "call_read", name: "read", input: { path: "{{work}}/hello.txt" } } },
    { text: "done" },
  ],
  steps: [
    { http: { method: "POST", path: "/api/session", body: {}, as: "session" } },
    { http: { method: "POST", path: "/api/session/{{session.data.id}}/prompt", body: { text: "read hello.txt" } } },
    { waitEvent: { type: "session.execution.succeeded" } },
    { files: { "notes.txt": "written while the server runs\n" } },
    { http: { method: "POST", path: "/api/pty", body: {}, as: "pty" } },
    { ws: { path: "/api/pty/{{pty.data.id}}/connect", receive: 1 } },
  ],
})
```

Every step kind has a field reference in [Steps](#steps). In short:

- `{ http: { ... } }` sends one request and saves its parsed body with `as`.
- `{ waitEvent: { type, match? }, timeout? }` waits for a matching event without adding a captured step.
- `{ files: { path: contents } }` writes files below `work` during the run without adding a captured step.
- `{ ws: { path, send?, receive, ... } }` opens a WebSocket, sends text frames and captures `receive` frames.

Templating resolves `{{work}}`, `{{root}}` and `{{<as>.<dotted.path>}}` in paths, query values, header values and the string leaves of request bodies and `match` objects; an unresolved placeholder fails the scenario.

`model` scripts the responses of the endpoint described in [Scripted model](#scripted-model). Each entry is a text reply `{ text: "..." }`, one tool call `{ tool: { id, name, input } }`, or a recorded interaction replayed verbatim `{ cassette: "packages/ai/test/fixtures/recordings/.../<name>.json", interaction: 0 }`. Replies are consumed one per request and must all be used.

Run one scenario by passing its file: `bun run parity --candidate ts parity/scenarios/prompt-text.scenario.ts`. Before committing a new scenario, check that it is deterministic with `bun run parity --candidate ts --repeat 20 parity/scenarios/<name>.scenario.ts`; both sides run the same TypeScript server, so a difference there is a harness or capture problem, not a candidate difference.

Add a deviation only when the difference is intended and the owner has accepted it: create a file under `parity/deviations` whose `Accepted:` line names the owner and date and whose `parity` block lists the rules (see [Deviations](#deviations)). Until then the difference fails the scenario.

## Testing a candidate

`--candidate` accepts `ts` or the path to an executable that speaks the same stdio process contract as the TypeScript server. The harness starts it as `<candidate> serve --stdio --port 0` with the [sandbox environment](#sandbox-and-environment), so it must:

- accept `serve --stdio --port 0` and serve the HTTP API on an ephemeral port;
- stream `GET /api/event`; the harness opens the event stream before the first step, so a candidate whose event stream is not OK stops the scenario with exit code `2`;
- read the password from `OPENCODE_PASSWORD` (`parity` during a run) and accept it through Basic auth and `auth_token`;
- print one JSON line such as `{"url":"http://127.0.0.1:41234"}` as its first stdout line, before it serves requests;
- exit when stdin closes; the harness closes stdin to stop it and escalates to `SIGTERM`, then `SIGKILL`, if it does not.

A relative path is resolved from the current directory. `--reference` accepts the same forms and defaults to `ts`, which runs the TypeScript server from `packages/cli`.

## Sandbox and environment

Each side pair gets a fresh random root created with `mkdtemp` under `PARITY_SANDBOX_DIR` (default `/tmp/opencode-parity`). The root contains fixed `home`, `home/.config`, `home/.local/share`, `home/.local/state`, `home/.cache`, `tmp` and `work` directories. The reference runs first, then `resetSandbox` removes the root and recreates those directories before the candidate runs, so neither side sees the other's state. Scenario `files` are written below `work`, the server runs with `work` as its working directory, and requests default to the `work` directory. The `bun-cache` directory next to the sandbox root is kept on purpose: it is the shared transpiler cache and is never captured. `SIGINT`, `SIGTERM` and `SIGHUP` kill the running servers, remove the sandbox root and exit with `128 + <signal>`.

Before the first step, each side opens a throwaway event stream, requests `/api/config`, waits for the session search index to settle, and then waits for that Location load to go quiet before closing the throwaway stream. The pre-warm stops a prompt from racing the index build, which would otherwise leave the SQLite full-text tables and the `session.search` progress row in a layout that depends on timing rather than behavior. Discarding the throwaway frames keeps `events` starting at `server.connected` without load-time noise; the real capture stream opens only after the warm-up has closed.

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
- `OPENCODE_CONFIG_CONTENT`: inline JSON that disables updates and the title agent, defines the `openai-test` and `anthropic-test` providers and their `openai-test/test-model` default (see [Scripted model](#scripted-model)), and is merged with the scenario's `config`.
- `BUN_RUNTIME_TRANSPILER_CACHE_PATH`: a shared cache next to the sandbox root, so each run does not transpile from scratch.

The harness also reads these variables from its own environment:

- `PARITY_SANDBOX_DIR`: parent directory for sandbox roots, default `/tmp/opencode-parity`.
- `PARITY_OUT_DIR`: output directory, default `parity/out`; the tests set it so they never overwrite a real run's output.
- `PARITY_START_TIMEOUT`: milliseconds to wait for a server's first stdout line, default `60000`.
- `PARITY_CANDIDATE_ENV`: JSON object of extra string variables applied to the candidate side only.

## Scripted model

A scenario that prompts the server needs somewhere for the model request to go. Each scenario starts one `Bun.serve` endpoint on a random `127.0.0.1` port before the reference side starts and stops it after the scenario finishes. Both sides receive the identical provider configuration, so the same scripted conversation is played against each side.

The sandbox base config defines two providers that point at the endpoint: `openai-test` with package `@ai-sdk/openai-compatible`, and `anthropic-test` with package `@ai-sdk/anthropic`. Both use settings `{ "apiKey": "parity", "baseURL": "<endpoint>/v1" }` and one model `test-model` with text in, text out, tools enabled, zero cost, and a `100000` token context with a `10000` token output limit. The default model is `openai-test/test-model`.

A scenario's `config` deep-merges over that base, so `config: { "model": "anthropic-test/test-model" }` selects the Anthropic route.

A scenario's `model` is an ordered reply script, consumed one reply per accepted request. It is reset before each side, so each side sees the same script. Replies are:

- `{ "text": "..." }`: a text response.
- `{ "tool": { "id": "...", "name": "read", "input": { ... } } }`: a single tool call.
- `{ "cassette": "packages/.../recording.json", "interaction": 0 }`: replays one interaction of an `@opencode/http-recorder` cassette, returning its status, headers and body verbatim. `interaction` defaults to `0` and indexes only the interactions with `transport: "http"`, and a base64-encoded body is decoded before it is returned.

The endpoint accepts `POST /v1/chat/completions` as OpenAI chat-completions chunks and `POST /v1/messages` as Anthropic Messages frames, and encodes a `text` or `tool` reply for whichever route the server called. Any other request returns 404 and is logged. A request after the script is exhausted returns 500 `{"error":"unscripted model request"}`.

`{{work}}` and `{{root}}` are replaced in `text` replies and in the string leaves of a tool reply's `input`. Every request is recorded in the capture's `model` array as `{ method, path, body }`, with the pathname only and the parsed JSON body. A reply left unused after a side fails the scenario with exit code `2` and names the count.

The base config disables the `title` agent, because title generation would send a second, concurrent model request that is not part of the scripted conversation, and sets `OPENCODE_FLAGS` to `-full_codemode`, because Code Mode runs `read` and `shell` inside `execute`, so a scripted `read` tool call would not appear as a direct tool. A scenario's `config` deep-merges, so it can re-enable the title agent with `agents: { "title": { "disabled": false } }` or set another flag through `env`, but doing so requires scripting the extra model calls.

## Normalisation rules

Rules apply to every string, number and object key in a capture.

| id | Replaces | Why |
| --- | --- | --- |
| `root` | The sandbox root and its URL-encoded form with `<root>`. | Both sides reuse the same root after a reset; the root is random per scenario, and absolute paths appear in URLs and payloads. |
| `port` | `<host>:<port>` for loopback `127.0.0.1:<port>` and `localhost:<port>`. | Each server binds an ephemeral port that differs between sides. |
| `id` | Prefixed ids with `<prefix:n>` using first-seen numbering per capture. | Freshly generated session and message ids differ but should play the same role. |
| `slug` | Session slugs with `<slug:n>`, numbered per first appearance and kept separate from ids. | The server picks a random human-readable slug per session. |
| `time` | Numbers in the capture window with `<time>`, ISO-8601 substrings with `<time:iso>` and all-digit strings in the window with `<time:digits>`. | Wall-clock values differ between the reference and candidate runs, and keeping the three forms distinct makes a type change a difference. |
| `process` | A numeric `pid` and a string `hostname` with `<pid>` and `<hostname>`. | Process ids differ per side and hostnames are machine specific. |
| `base64` | Base64 strings that decode to a JSON object or array are decoded, normalised recursively, and re-encoded. | Session-list cursors encode session ids and timestamps. |

Strings that look like JSON objects or arrays are parsed only when the value re-serialises exactly with compact, 2-space, tab or 4-space indentation (trailing whitespace allowed), then normalised and re-serialised with the same formatting; anything else is left as text with the string rules applied, so formatting and number-precision differences remain visible. Object keys below the top level are visited in sorted order, while the top level keeps capture order (`steps`, `events`, `logs`, `model`, `database`, `files`), so id numbering does not depend on key order. Object keys are normalised too, so a `logs` map keyed by a raw session id becomes the same `<ses:n>` placeholder as its value.

## Captures

A capture is `{ steps, events, logs, model, database, files }`.

- `steps`: one entry per `http` or `ws` step. An `http` entry is `{ status, headers, body }`. `status` is the numeric response status. `headers` contains lower-cased header names sorted alphabetically, dropping `date`, `connection`, `keep-alive`, `content-length`, `transfer-encoding`, `etag` and `last-modified`. `body` is parsed as JSON when the response content type contains `json` and kept as text otherwise, and a JSON-typed body that is empty or invalid is also kept as text, so a 401 error body is captured but never asserted by the scenario itself. A `ws` entry is `{ opened, frames, close }`, where `opened` is whether the socket opened, `frames` holds text frames in arrival order with binary frames encoded as `base64:<data>`, and `close` is `{ code, reason }` or `null`. If the receive count is not reached before the timeout the entry also has `timedOut: true`; a timeout is captured for comparison and is not a scenario error.
- `events`: every `/api/event` frame collected in arrival order. The stream opens before the first step and closes after the last, so it starts with `server.connected`. A frame whose data is not JSON is kept as `{ "invalid": "<raw>" }`.
- `logs`: for each session created in the run, the parsed frames of `GET /api/experimental/session/<id>/log`, keyed by the raw session id. A non-OK response is captured as `[{ status, body }]`.
- `model`: outbound model requests, one `{ method, path, body }` entry per request the scripted endpoint received, holding the pathname only and the parsed JSON body. Both sides are compared.
- `database`: one entry per SQLite file under the sandbox, keyed by its relative path, captured after the server has exited. Each entry has `schema` (`type`, `name`, `tbl_name`, `sql`, sorted by type then name) and `tables` (one array of row objects per table). Blob values are `hex:<lowercase hex>`, and table rows are sorted by their serialised JSON after normalisation. The sort does not by itself erase insertion order: ids already numbered from `steps`, `events`, `logs` or `model` keep those numbers, while ids that first appear in a database row are numbered in row order before the sort.
- `files`: every other file under the sandbox as relative path to contents, captured after the server has exited. Valid UTF-8 files store their text and other files store `sha256:<hex>`. Excluded are SQLite files and their `-wal`, `-shm` and `-journal` siblings (captured as `database` instead), `models.json` (written by the harness), and `home/.local/share/opencode/log/**` (log lines carry wall-clock times, run ids and process ids).

Each run writes three files below `parity/out/<scenario>/`: `reference.json` and `candidate.json` hold the normalized captures, and `differences.json` holds the compared paths with their reference and candidate values.

## Coverage

After a run that captured at least one scenario, the harness writes `parity/out/coverage.md` and `parity/out/coverage.json` and prints one line: `coverage: operations <a>/<total>, events <b>/<total>, websockets <c>/<total>`.

The surface is read from `packages/protocol/openapi.json` at the commit in `packages/server-rs/upstream-pin`. When that document cannot be read at the pin, the checkout copy is used and a warning names the pin. Operations are the OpenAPI HTTP operations, events are the `types` enum values of `GET /api/event` plus `server.connected` and the `rpc.*` pattern, and WebSocket routes are operations marked `x-websocket`.

Coverage is measured per reference-side scenario. An `http` step covers the operation whose template matches its path without the query string, a `ws` step covers the matching WebSocket route, and captured event types cover their event, with any `rpc.` type covering `rpc.*`. Request paths that match no operation or WebSocket route are listed as unmatched.

## Comparison modes

By default every part of the capture is compared: `steps`, `events`, `database`, `files`, `logs` and `model`.

`--only <parts>` restricts what is compared to a comma list of those parts; omitting it selects all six, and repeated names are collapsed. It never changes what a scenario runs: every step, including `http`, `ws`, `files` and templating, still executes exactly as before. When `events` is not selected, neither side opens `/api/event` (not even the throwaway warm-up stream), the per-session log fetch is skipped so `logs` stays `{}`, and a scenario that contains a `waitEvent` step is refused before any sandbox is created with `FAIL <scenario>: scenario needs events` and exit 2. `logs` requires `events` because the log keys come from sessions discovered on the event stream, and selecting `logs` without `events` fails with exit 2.

`--tables <names>` restricts the `database` comparison to those tables in every captured database file. It also filters the compared schema rows to those tables, including index rows, so an index whose `tbl_name` is not selected is dropped. A name that matches no captured table fails the scenario with exit 2 and `--tables <names> matched no table`, which catches a typo that would otherwise compare nothing.

Coverage only counts the parts being compared. An uncompared section keeps `covered: false` and no scenarios, is marked `not compared` instead of `yes`/`no` in `coverage.md`, gains `"compared": false` in `coverage.json`, and prints `events not compared` (for example) instead of a fraction in the summary line. WebSocket routes are covered by `ws` steps, so that section is compared only when a compared step opened one; a run without WebSocket steps marks it `not compared`. The normalized `reference.json` and `candidate.json` captures are still written in full; only `differences.json` reflects the restricted comparison.

## Deviations

Every difference from the TypeScript server must be covered by a file in the deviations directory (default
`packages/server-rs/parity/deviations`, overridable with `--deviations`). A file explains nothing until its
`Accepted:` line has a non-empty owner and date; a value starting with `pending` in any case is not accepted, so the
current ledger of `Accepted: pending owner` files explains nothing and only produces warnings.

Without `--allow-pending`, a pending file explains nothing and only warns. With `--allow-pending`, pending files are
loaded and matched like accepted ones, so they can check a candidate against deviations that wait for owner
acceptance. A pending file with no `parity` block explains nothing, even with `--allow-pending`, and a scenario whose
differences a pending deviation explains prints `PASS <scenario> (<n> explained by pending <ids>)`. CI never passes
the flag, so a pending deviation still counts as a difference there.

A scenario passes when every difference matches an accepted deviation. It prints
`PASS <scenario> (<n> explained by <ids>)`; a difference that matches no accepted deviation still fails the scenario
as before.

An accepted file must contain a fenced code block with the info string `parity`, one rule per line:
`<scenario glob> <difference-path glob>`. Globs use `Bun.Glob` semantics, the scenario name is its path under
`parity/scenarios` without `.scenario.ts`, and the difference path uses the capture paths (`steps/<i>/body/...`,
`events/<i>/...`, `logs/<name>/...`, `model/<i>/...`, `database/<file>/tables/<table>/...`, `files/<path>`); a `ws`
step's paths start with `steps/<i>/opened`, `steps/<i>/frames` or `steps/<i>/close`.

An accepted file with no `parity` block fails the run with exit 2. After a full run with no scenario arguments,
accepted deviations that explained nothing print `warning: deviation <id> matched nothing`, and files without an
accepted line print `warning: deviation <id> is not accepted`. The file stem is the deviation id shown in these
messages.

## Exit codes

- `0`: every scenario matched.
- `1`: at least one scenario reported a difference that no accepted deviation explains.
- `2`: a scenario, start, configuration or ledger error, such as a server that did not start, an invalid flag value, an
  unreadable cassette or a missing cassette interaction, unused model replies, an event stream that fails or ends while
  a `waitEvent` waits, a search index that does not settle within 30 s, or an accepted deviation file with no `parity`
  block. Any unexpected harness error prints `parity: <message>` and
  exits `2`.
- `128 + <signal>`: the harness received `SIGINT`, `SIGTERM` or `SIGHUP`.
