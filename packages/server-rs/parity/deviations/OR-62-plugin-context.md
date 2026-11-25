# OR-62: plugins receive only part of the plugin context in the sidecar

TypeScript: `packages/core/src/plugin/host.ts:128` builds every plugin's full context (`app`, `location`,
`options`, `rpc`, `agent`, `aisdk`, `command`, `credential`, `event`, `experimental`, `generate`, `integration`,
`mcp`, `model`, `permission`, `provider`, `reference`, `session`, `shell`, `skill`, `storage`, `tool`, `vcs`,
`websearch`, `worktree`), so a plugin can call any of those services.

Rust: `host/src/plugin/context.ts` gives a plugin only `app`, `location`, `options` and `plugin.list`; every other
domain is a proxy that dies with `<path> is not available in the sidecar yet (OR-63)`. The test
`host/test/plugin-load.test.ts` covers the available fields and that message.

Reason: the full context is OR-63's scope; this task only needs `plugin.list` for activation.

Accepted: pending owner
