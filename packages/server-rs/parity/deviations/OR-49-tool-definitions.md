# OR-49: the model request omits TypeScript's skill tool definition when every tool is denied

TypeScript sends the built-in `skill` tool definition even when the agent's permissions deny every tool, so the model
request body has a `tools` array containing the skill entry. `packages/core/src/session/context.ts` builds the tool
snapshot and `packages/core/src/tool/model-policy.ts` filters it; the parity reference for `prompt-text-only` shows one
`skill` tool with `cache_control`.

Rust: `crates/session/src/model_request.rs` currently always sends no tools, because the tool registry and
permission filtering arrive with OR-51.

Reason: the skill tool description is registry-owned; there is no tool to describe until OR-51 lands.

Accepted: pending owner

```parity
prompt-text-only model/*/body/tools
prompt-text-only model/*/body/tools/**
```
