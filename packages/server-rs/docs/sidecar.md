# The JavaScript sidecar

Plugins are JavaScript modules, so the server runs them in a Bun child process instead of its own. This document
describes that process as `opencode-sidecar` (`crates/sidecar`) implements it.

## Overview

There is one sidecar per server process. `Running` in `crates/server/src/serve.rs` owns a single `Sidecar`, made from
the server's variables (`ServerEnv`), its working directory and the temporary directory it already uses.

The sidecar is lazy: `Sidecar::new` starts nothing, and the first call or stream starts Bun, which then stays up until
shutdown. Concurrent first callers share that one start. The server and the child talk over a socket placed on the
child's fd 0, so the sidecar reads its input from stdin and needs no listening socket. The child's stdout and stderr
are forwarded line by line to the server's stderr with a `sidecar: ` prefix, and the last 20 lines are kept for error
messages.

## Frames

Every message is one frame: a 5-byte header, then the payload.

| Offset | Bytes | Meaning                             |
| ------ | ----- | ----------------------------------- |
| 0      | 4     | payload length, `u32` little-endian |
| 4      | 1     | kind: `0` JSON, `1` BINARY          |

Kind `0` carries the message alone. Kind `1` carries a 4-byte little-endian JSON length, that much JSON, then the raw
bytes attached to the message; the outer length counts those four bytes.

`frame::read` checks the length against `max_frame` before it allocates the payload, so a header that claims 4 GiB
costs nothing. A frame cut short, an unknown kind, a frame over its cap and JSON that does not parse all break the
connection (`frame::Broken`): nothing after such a frame can be trusted, and pending calls and streams fail.

`frame::DEFAULT_MAX_FRAME` is 16 MiB. The server sends its own cap in `hello` and the host adopts it, so both sides
encode and decode against the negotiated value.

## Messages

The Rust types in `crates/sidecar/src/protocol.rs` are the definition. `VERSION` is `1` and `MAGIC` is
`opencode-sidecar`: a `ready` naming another pair is refused, and the handshake fails.

The wire form is tagged by a `type` field in kebab-case, with camelCase fields. `tests/all/bindings.rs` generates
`host/src/protocol.gen.ts` from those types through `ts-rs`; regenerate it with `OPENCODE_UPDATE_SIDECAR_PROTOCOL=1`
and never edit it by hand.

| `type`   | Fields                                                     | Sent by                                |
| -------- | ---------------------------------------------------------- | -------------------------------------- |
| `hello`  | `version`, `maxFrame`, `window`, `heartbeatMs`, `test`     | the server, first                      |
| `ready`  | `magic`, `version`, `pid`                                  | the host, in answer                    |
| `call`   | `id`, `method`, `location?`, `parent?`, `params`, `stream` | either side                            |
| `result` | `id`, `value`                                              | the callee                             |
| `error`  | `id`, `error`                                              | the callee                             |
| `cancel` | `id`                                                       | either side                            |
| `chunk`  | `id`, `value`                                              | the callee, on a stream                |
| `credit` | `id`, `chunks`                                             | the caller, on its own stream call     |
| `end`    | `id`                                                       | the callee, on a stream                |
| `ping`   | `seq`                                                      | the server                             |
| `pong`   | `seq`, `rss`                                               | the host                               |
| `log`    | `level`, `message`                                         | the host, for a line the server prints |

`hello.version` is the protocol version, `hello.maxFrame` the cap both sides keep, `hello.window` the stream credit in
chunks, and `hello.heartbeatMs` the interval at which the server pings. `hello.test` asks the host to also register
its test-only methods; the server leaves `Config::test_methods` off.

`ready.pid` is the child's process id and `pong.rss` its resident set size in bytes. `call.params` and the `value` of
`result` and `chunk` are arbitrary JSON. `call.parent` names the call that caused this one when the host calls back
into the server. A stream `call` carries `stream: true`, and its `chunk`s are followed by either `end` or `error`.

Two types appear inside messages:

- `LocationRef { directory, workspaceID? }` names the directory a call works for; `workspaceID` is omitted unless it
  is set.
- `RemoteError { _tag, message, data? }` is an error crossing the process boundary; `_tag` is its name, `message` its
  text, and `data` optional JSON. `LogLevel` is `debug`, `info`, `warn` or `error`, in lowercase.

## Call ids

Each side keeps its own counter, starting at 1, and only ever refers to its own numbers when naming a call it starts.
`call` and `cancel` name the sender's own id. `result`, `error`, `chunk` and `end` name the caller's id, not the
callee's. `credit` is sent by the caller for its own stream call, so its `id` is the caller's as well. A `cancel` is
best effort: the receiver drops the call, and a reply already on its way is ignored because the pending entry is
gone.

A stream starts with the caller sending `credit { chunks: window }`. Each chunk sent spends one credit, and the caller
grants one back for each chunk it consumes. A `chunk` beyond the granted credit breaks the connection.

## Limits

`Limits` in `crates/sidecar/src/config.rs` holds every bound, with these defaults:

| Field             | Default | Meaning                                                                                 |
| ----------------- | ------- | --------------------------------------------------------------------------------------- |
| `max_frame`       | 16 MiB  | largest frame either side accepts; sent in `hello`                                      |
| `queued_bytes`    | 8 MiB   | byte budget for frames waiting to be written, raised to `max_frame` when that is larger |
| `max_outgoing`    | 1024    | most calls and streams the server keeps pending; beyond it a call fails as busy         |
| `max_incoming`    | 256     | most calls from the host served at once; beyond it the host gets a `Busy` error         |
| `window`          | 16      | stream credit in chunks                                                                 |
| `handshake`       | 10 s    | how long a start waits for `ready`                                                      |
| `heartbeat`       | 5 s     | interval of the server's `ping`                                                         |
| `liveness`        | 15 s    | longest silence without a `pong` before the process is killed                           |
| `memory`          | 1 GiB   | largest `pong.rss` allowed                                                              |
| `backoff_initial` | 250 ms  | delay after the first crash                                                             |
| `backoff_max`     | 30 s    | cap on the delay between restarts                                                       |
| `crash_limit`     | 5       | crashes in a row that disable the sidecar                                               |
| `crash_window`    | 60 s    | uptime after which a crash counts as the first                                          |
| `grace`           | 500 ms  | how long shutdown waits for an exit before killing the process group                    |

## Supervision

`Sidecar` in `crates/sidecar/src/supervisor.rs` owns the child. Its slot is `Idle`, `Starting`, `Ready`, `Backoff`,
`Disabled` or `ShuttingDown`, and `Sidecar::state` reports the public `Idle`, `Ready`, `Exited` and `Disabled` states
as they change.

- The first call or stream takes `Idle`, or an expired `Backoff`, into `Starting` and runs the start in its own task.
  Every other caller that arrives meanwhile waits on the same result instead of starting another process.
- `Ready` holds one generation. The monitor task pings it every `heartbeat` and records each `pong`.
- A `pong.rss` above `memory` ends the generation with `MemoryExceeded`. No `pong` within `liveness` of the last one,
  or no answer to a `ping` within `liveness`, ends it with `Unresponsive`. Either way the process group is killed.
- A crash after `crash_window` of uptime counts as the first; otherwise the count grows. The delay is
  `backoff_initial * 2^(crashes - 1)`, capped at `backoff_max`, and `crash_limit` crashes in a row leave the sidecar
  `Disabled` until a new `Sidecar` is made. A start that fails after a runtime is found, in spawning it or setting up
  the socket, counts as a crash too, so a broken runtime backs off instead of retrying in a hot loop; a runtime that
  cannot be found returns to `Idle` without counting one.
- Every generation has one typed exit reason: `Exited` with the status, signal and last stderr lines, or
  `Unresponsive`, `MemoryExceeded` or `ShuttingDown`. The connection reports its own end first, and a failing call or
  stream waits briefly for the process's real reason so the caller sees that instead.
- `Sidecar::shutdown` refuses later calls with `ShuttingDown`, closes the write end of the socket, gives the host
  `grace` to exit, then kills the process group and waits `grace` again. The host exits on the end of its input and
  kills its own descendants; the group kill is the fallback for a host that does not. Dropping the last handle closes
  the write end and kills the group as well.

## Finding Bun

`crates/sidecar/src/bun.rs` looks for a runtime in this order, and never takes one from the project directory:

1. `OPENCODE_BUN`, when it is an absolute path, is used as given.
2. The bundled `opencode` beside the current executable, when it is outside the project directory, is asked whether
   its Bun is new enough and is run with `BUN_BE_BUN=1`.
3. Each `opencode` on `PATH`, in an absolute entry only, is tried the same way. The path is canonicalized first,
   because `opencode` on the path may be a version-manager shim that answers for itself instead of the binary it
   fronts.
4. `bun` on `PATH`, in an absolute entry only and outside the project directory.

Relative and empty `PATH` entries are skipped: they are the working directory, so they are the project's own tools.
A project that happens to be the home directory counts as no project, and a runtime there is accepted.

The probe for candidates 2 to 4 runs `<binary> --version` in the binary's own directory with a cleared environment,
`BUN_BE_BUN=1`, only the variables in `NAMES`, and a one-second timeout. It must exit successfully and print one
`major.minor.patch` version (an optional `-` or `+` suffix allowed). Candidates 2 and 3 must answer 1.4.0 or newer,
`bun::OLDEST_BUN`; the `OPENCODE_BUN` override in 1 is not checked, and for a plain `bun` in 4 the probe is advisory:
the binary is taken either way and the version is only recorded when it answers.

## Environment and working directory

The child starts with `env_clear`, so it sees only the variables `config::inherited` allows: the `NAMES` list, any
`LC_*` and any `XDG_*`, compared case-insensitively. Everything else in the server's environment stays there. In
particular no `OPENCODE_*` variable is passed, so `OPENCODE_PASSWORD`, `OPENCODE_SERVER_PASSWORD` and the rest of the
server's configuration never reach a plugin. Node and Bun options are excluded for the same reason: they could
preload code.

`NAMES` covers, by group:

- terminal and colour: `TERM`, `TERM_PROGRAM`, `TERM_PROGRAM_VERSION`, `COLORTERM`, `COLORFGBG`, `NO_COLOR`,
  `FORCE_COLOR`, `TMUX`, `TMUX_PANE`, `STY`, `TERMINAL_EMULATOR`, `VTE_VERSION`, `KONSOLE_VERSION`,
  `ITERM_SESSION_ID`, `ITERM_PROFILE`, `WT_SESSION`, `WT_PROFILE_ID`, `WEZTERM_EXECUTABLE`, `WEZTERM_PANE`,
  `KITTY_WINDOW_ID`, `KITTY_PID`, `KITTY_INSTALLATION_DIR`, `GHOSTTY_RESOURCES_DIR`, `GHOSTTY_BIN_DIR`,
  `ALACRITTY_WINDOW_ID`, `CONEMUANSI`, `WSL_DISTRO_NAME`, `SSH_TTY`
- user and paths: `PATH`, `HOME`, `USER`, `LOGNAME`, `SHELL`, `LANG`, `TZ`
- temporary directories: `TMPDIR`, `TEMP`, `TMP`
- proxies: `HTTP_PROXY`, `HTTPS_PROXY`, `NO_PROXY`, `ALL_PROXY`
- Windows: `SYSTEMROOT`, `WINDIR`, `COMSPEC`, `PATHEXT`, `USERPROFILE`, `APPDATA`, `LOCALAPPDATA`, `PROGRAMDATA`,
  `SYSTEMDRIVE`

`crates/sidecar/src/spawn.rs` makes a private temporary directory under the server's temporary directory, mode `0700`,
and writes `host.js` there from the bundle compiled into the binary together with a `bunfig.toml` that disables
installs. Bun runs in that directory, not in the project, so a plugin sees a scratch working directory and a project
`.env` is not read: the child runs `--no-env-file --no-install --config=<that bunfig.toml> <that host.js>`. The child
is put in its own process group, which is what shutdown and a dropped handle kill.

## Measurements

Measured on 2026-11-23 from a release build in the development sandbox with `bun run measure:sidecar`, which runs
`cargo test --release -p opencode-sidecar --test all measure -- --ignored --nocapture`. These are single-run figures:
the memory rows move by several MiB between runs as Bun collects garbage.

| Measurement                              | Value                |
| ---------------------------------------- | -------------------- |
| Bun                                      | 1.4.4                |
| Platform                                 | linux x86_64, 4 CPUs |
| Memory, idle: RSS / peak                 | 26.5 MiB / 26.5 MiB  |
| Memory, Effect loaded: RSS / peak        | 62.8 MiB / 62.8 MiB  |
| Memory, sample plugin loaded: RSS / peak | 74.0 MiB / 74.0 MiB  |
| Latency, small echo: p50 / p99           | 0.03 ms / 0.06 ms    |
| Latency, 1 MiB echo: p50 / p99           | 2.55 ms / 5.65 ms    |
