# First turn from the real clients

`test/shell/first-turn.test.ts` proves that each shipped client can drive a complete first turn against the
Rust server, and that the conversation survives a server restart. `bun run test:shell` from `packages/server-rs`
runs it (all four cases, no skips) together with the rest of the shell suite.

The automated cases use a sandboxed home and a local model endpoint that replays
`packages/ai/test/fixtures/recordings/anthropic-compatible-messages/minimax-m3-anthropic-compatible-text.json`,
which answers `Hello!`. The manual steps below reproduce the same flow by hand.

## Build

```sh
bun run --cwd packages/server-rs cargo build -q -p opencode-server
bun run --cwd packages/cli-rs cargo build -q --bin opencode-native
```

The Rust server is `packages/server-rs/target/debug/opencode-server`; the native client is
`packages/cli-rs/target/debug/opencode-native`.

## Environment

Use a scratch home so nothing touches your real configuration, and point it at a provider that answers text:

```sh
export SCRATCH=$(mktemp -d)
export HOME="$SCRATCH/home"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_DATA_HOME="$HOME/.local/share"
export XDG_STATE_HOME="$HOME/.local/state"
export XDG_CACHE_HOME="$HOME/.cache"
mkdir -p "$XDG_CONFIG_HOME/opencode" "$XDG_DATA_HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"
export OPENCODE_PASSWORD=dev
export OPENCODE_DISABLE_MODELS_FETCH=1
```

Write `$XDG_CONFIG_HOME/opencode/opencode.json` with the provider and model the clients should use. For a
recorded run this is the parity test provider; for a manual run any provider with text output works, for
example:

```json
{
  "model": "anthropic-test/test-model",
  "agents": { "build": { "permissions": [{ "action": "*", "resource": "*", "effect": "deny" }] } },
  "providers": {
    "anthropic-test": {
      "name": "Anthropic Test",
      "package": "@ai-sdk/anthropic",
      "settings": { "apiKey": "dev", "baseURL": "http://127.0.0.1:4000/v1" },
      "models": {
        "test-model": {
          "name": "Test Model",
          "capabilities": { "tools": true, "input": ["text"], "output": ["text"] },
          "cost": { "input": 0, "output": 0 },
          "limit": { "context": 100000, "output": 10000 }
        }
      }
    }
  }
}
```

Run the parity model endpoint if you want the exact reply:

```sh
bun -e 'import { startModelEndpoint } from "/path/to/packages/server-rs/parity/lib/model.ts"
const endpoint = await startModelEndpoint()
endpoint.load([{ cassette: "packages/ai/test/fixtures/recordings/anthropic-compatible-messages/minimax-m3-anthropic-compatible-text.json" }], { work: process.cwd(), root: process.cwd() })
console.log(endpoint.url)
await new Promise(() => {})'
```

Put that URL in `baseURL` (with `/v1`).

## Restart

Run the server from the project directory you want the session in, so the location is that directory:

```sh
/path/to/packages/server-rs/target/debug/opencode-server serve --stdio --port 0
```

It prints `{"url":"http://127.0.0.1:<port>"}`. In another shell (with the same environment):

```sh
URL=http://127.0.0.1:<port>
AUTH="opencode:$OPENCODE_PASSWORD"
curl -su "$AUTH" -H "x-opencode-directory: $PWD" -H 'content-type: application/json' \
  -d '{}' "$URL/api/session"
ID=<data.id from the response>
curl -su "$AUTH" -H "x-opencode-directory: $PWD" -H 'content-type: application/json' \
  -d '{"text":"say hi"}' "$URL/api/session/$ID/prompt"
curl -su "$AUTH" -H "x-opencode-directory: $PWD" -X POST "$URL/api/experimental/session/$ID/wait"
```

Stop the server by closing its standard input (Ctrl-D in the terminal, or the test's stdin end). Start it
again the same way and read the history:

```sh
curl -su "$AUTH" -H "x-opencode-directory: $PWD" "$URL/api/session/$ID/message"
```

The response still holds the user message `say hi` and an assistant message whose text part is `Hello!`,
because history comes from the database rather than process memory.

## TypeScript `run`

Start the server with a fixed port and note the URL it prints:

```sh
/path/to/packages/server-rs/target/debug/opencode-server serve --port 0
```

From the project directory, with the same environment:

```sh
bun --preload=$(bun -e 'console.log(Bun.resolveSync("@opentui/solid/preload", "/path/to/packages/cli"))') \
  /path/to/packages/cli/src/index.ts run --server "$URL" "say hi"
```

It exits 0 and prints `Hello!`. (The client sends `x-opencode-directory` for its working directory, so run it
from the same project directory as the server.)

## Native CLI

With `OPENCODE_TS_BIN` pointing at the Rust server binary, the native client starts its own server:

```sh
OPENCODE_TS_BIN=/path/to/packages/server-rs/target/debug/opencode-server \
  /path/to/packages/cli-rs/target/debug/opencode-native run --standalone "say hi"
```

It exits 0 and prints `Hello!`.

## TypeScript TUI

Start the port server as above, then run the TUI under a pseudo-terminal with a startup prompt:

```sh
script -qfec "bun --preload=... /path/to/packages/cli/src/index.ts --server $URL --prompt 'say hi'" /dev/null
```

The TUI completes the turn; the session history on the server holds `say hi` and `Hello!`. This also shows
that its startup calls succeed: `server.info`, `event.subscribe`, `location.get`, `agent.list`, `model.list`,
`session.create`, `session.switchModel` and `session.prompt`.

## What the automated test checks

- **Restart:** one stdio server, prompt, idle, stdin closed, a second stdio server on the same sandbox, and
  the message list read from the new process.
- **TypeScript `run`:** the Rust server on a port and the CLI subprocess, asserting exit 0, `Hello!` on
  stdout, no unscripted model request and no model endpoint failure.
- **Native CLI:** the built `opencode-native` under `run --standalone` with `OPENCODE_TS_BIN` set to the Rust
  binary, asserting the same. The case fails with the build command when the binary is missing rather than
  skipping.
- **TypeScript TUI:** the TUI under `script -qfec`, polling the server until an assistant message reads
  `Hello!`, then killing the pseudo-terminal.
