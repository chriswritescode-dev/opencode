# OR-47: the SAP AI Core and Claude Agent provider plugins are not registered in Rust

TypeScript: `packages/core/src/plugin/provider.ts` registers 30 provider plugins in `ProviderPlugins`. Two of them are `opencode.provider.sap.ai.core` (`sap-ai-core.ts`, whose behaviour is AI SDK hooks) and `opencode.provider.claude-agent` (`claude-agent/`, which adds the `claude-agent` integration and provider).

Rust: `crates/catalog/src/provider_plugin/mod.rs` lists these two ids in `ORDER` as `Slot::Elsewhere` and `provider_plugin::register` skips them, so the Rust server registers the 28 native plugins only. `GET /api/integration` omits the `claude-agent` integration, and `GET /api/provider` lacks the `claude-agent` provider edits these plugins make.

Reason: SAP AI Core and Claude Agent depend on the AI SDK sidecar seams that OR-64 owns. The `provider-plugin-integrations` parity scenario therefore requests each native plugin's integration by id instead of `GET /api/integration`.

Accepted: pending owner
