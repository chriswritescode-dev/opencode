# OR-47: the OpenCode Console, SAP AI Core and Claude Agent provider plugins are not registered in Rust

TypeScript: `packages/core/src/plugin/provider.ts` registers 30 provider plugins in `ProviderPlugins`. Three of them are `opencode.provider.opencode` (`packages/core/src/plugin/provider/opencode.ts`, which adds the Console sign-in methods to the `opencode` integration and edits the `opencode` provider and its models), `opencode.provider.sap.ai.core` (`sap-ai-core.ts`, whose behaviour is AI SDK hooks) and `opencode.provider.claude-agent` (`claude-agent/`, which adds the `claude-agent` integration and provider).

Rust: `crates/catalog/src/provider_plugin/mod.rs` lists these three ids in `ORDER` as `Slot::Elsewhere` and `provider_plugin::register` skips them, so the Rust server registers the 27 native plugins only. `GET /api/integration` omits the `claude-agent` integration and the Console methods of `opencode`, and `GET /api/provider` lacks the `opencode` and `claude-agent` provider edits these plugins make.

Reason: the OpenCode Console provider belongs to OR-48, and SAP AI Core and Claude Agent depend on the AI SDK sidecar seams that OR-64 owns. The `provider-plugin-integrations` parity scenario therefore requests each native plugin's integration by id instead of `GET /api/integration`.

Accepted: pending owner
