# Salvage inventory

The first attempt lives on the `main` and `rust-rewrite` branches of the private repository, which this checkout
cannot see, so the importing task resolves the first-attempt file paths.

ADR-004 of project.md `Decisions.md` is the source of this list. Nothing is copied into this package until the task
named in the row needs it.

| Module | First-attempt crate | What it provides | Target crate | Imported by | Check before import |
| --- | --- | --- | --- | --- | --- |
| process helpers | `oc-tools` | Spawning and supervising child processes, output capture and timeouts | `shell` | OR-52 | `packages/util/src/cross-spawn-spawner.ts`, `packages/util/src/execute-output.ts`; `opencode_util::job` only runs a foreground terminal program and waits for it |
| PTY | `oc-tools` | Pseudo-terminal allocation, resize and teardown | `shell` | OR-52 | the `packages/process-daemon` client protocol and `packages/core/src/pty/protocol.ts` |
| git | `oc-tools` | Git command wrappers and repository inspection | `vcs` | OR-53 | `packages/core/src/git.ts` |
| search | `oc-tools` | Ripgrep-backed file and content search | `fs` | OR-51 | `packages/core/src/filesystem/search.ts` |
| diff | `oc-tools` | Unified diff generation and parsing | `vcs` | OR-53 | `packages/core/src/session/diff.ts` |
| patch | `oc-tools` | Applying patches to files | `tool` | OR-51 | `packages/core/src/tool/plugin/patch.ts`, `packages/core/src/vcs/patch.ts` |
| watcher | `oc-tools` | Filesystem watcher with debounce | `fs` | OR-51 | `packages/core/src/filesystem/watcher.ts` |
| image helpers | `oc-tools` | Image decode, resize and format checks | `tool` | OR-51 | `packages/core/src/image.ts` |
| SigV4 signing | `oc-llm` | AWS request signing | `ai` | OR-44 | `packages/ai/src/protocols/utils/bedrock-auth.ts`; OR-44 ported from the TypeScript at the pin and imported no first-attempt code |
| AWS event-stream framing | `oc-llm` | Binary event-stream encoding and decoding | `ai` | OR-44 | `packages/ai/src/protocols/bedrock-event-stream.ts`; OR-44 ported from the TypeScript at the pin and imported no first-attempt code |
| SSE framing | `oc-llm` | Server-sent event encoding and decoding | `ai` | OR-44 | `packages/ai/src/route/framing.ts`; OR-44 ported from the TypeScript at the pin and imported no first-attempt code |
| transports | `oc-llm` | HTTP and WebSocket transports | `ai` | OR-44 | `packages/ai/src/route/transport/*`; OR-44 ported from the TypeScript at the pin and imported no first-attempt code |
| file lock | `oc-core` | Cross-process lock directories | `config` | OR-42 | compare with `opencode_local::lock` first and prefer it when it already matches; `packages/util/src/flock.ts` |
| OAuth callback page | `oc-core` | The local page shown after an OAuth redirect | `catalog` | OR-46 | `packages/core/src/oauth/page.ts` |
| migration runner | `oc-storage` | Applies ordered migrations to SQLite | `storage` | OR-40 | `packages/core/src/database/migration.ts`; OR-40 ported the TypeScript at the pin and imported no first-attempt code, because that branch is not visible in this checkout |
| dump tooling | `oc-storage` | Database dump and restore helpers | `storage` | OR-40 | the debug entry points for storage at the pin; OR-40 ported the dump format from the TypeScript at the pin and imported no first-attempt code, because that branch is not visible in this checkout |
| ID helpers | `oc-schema` | Prefixed, sortable identifiers | `storage` | OR-40 | `packages/schema/src/identifier.ts`; OR-40 ported the TypeScript at the pin and imported no first-attempt code, because that branch is not visible in this checkout |
| JavaScript-string helpers | `oc-schema` | String conversions that match JavaScript semantics | `contract` | OR-38 | OR-38 used `opencode_api_types::js` (`encode_component`, `decode_component`) and imported no first-attempt code: the helpers already match the TypeScript behaviour, and the first-attempt branches are not visible in this checkout. |
| number helpers | `oc-schema` | Number parsing and formatting that match JavaScript | `contract` | OR-38 | OR-38 used `opencode_api_types::js` (`number_to_string`) and imported no first-attempt code: the helpers already match the TypeScript behaviour, and the first-attempt branches are not visible in this checkout. |
| wildcard helpers | `oc-schema` | Glob-style pattern matching for permission rules | `session` permissions | OR-50 | `packages/core/src/util/wildcard.ts` |
| response validator | `oc-contract` | Validates responses against the committed schema | `parity/` | OR-37 | `packages/protocol/src/client.ts` and `packages/protocol/openapi.json` at the pin |
| git fixtures | `oc-testkit` | Temporary git repositories for tests | `vcs` tests | OR-53 | `packages/core/test/fixture/git.ts` |
| server scaffold | `oc-server` | Authentication, CORS, event stream and error scaffolding | `http` | OR-39 | no first-attempt code was imported because the first-attempt branches are not visible in this checkout, and OR-39 ported the modules from the pinned TypeScript instead |

## Not reused

ADR-004 excludes the first attempt's hand-written wire types, its table modules, its session crate, and its SDK, TUI
and CLI.

Each import re-checks the module against the TypeScript behaviour at the pinned commit, and records any difference as
a deviation in `../parity/deviations/`.
