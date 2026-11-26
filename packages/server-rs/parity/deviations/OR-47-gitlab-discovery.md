# OR-47: GitLab workflow discovery and the GitLab SDK hooks are not implemented in Rust

TypeScript: `packages/core/src/plugin/provider/gitlab.ts:95` imports `discoverWorkflowModels` from `gitlab-ai-provider` and calls it with the instance URL, the login headers, the Location directory and the credential id as the cache key. The returned workflow models become `duo-workflow-*` provider models with `package: aisdk:gitlab-ai-provider` and `settings.workflowRef`. The same package supplies the two `ctx.aisdk` hooks at `gitlab.ts:303` (`sdk`, which builds the client with the instance URL, token, `aiGatewayHeaders` and feature flags) and `gitlab.ts:328` (`language`, which routes `duo-workflow-*` models through `workflowChat` and other models through `agenticChat`).

Rust: `crates/catalog/src/provider_plugin/gitlab.rs` reaches discovery through the `provider_plugin::gitlab::Discovery` seam on `PluginContext` and ships `NoDiscovery`, which returns an error. The plugin logs `failed to discover GitLab workflow models` and publishes no workflow models, which is the same state TypeScript shows after a failed discovery. Provider and integration methods, OAuth login, refresh, the `instanceUrl` login settings and the discovered-model transform are ported. There is no `sdk` or `language` hook.

Reason: `gitlab-ai-provider` is a Node package that owns project detection, GraphQL discovery, caching and the chat client. OR-64 owns the sidecar integration that supplies this seam and the SDK hooks through `gitlab-ai-provider`. These `packages/core/test/plugin/provider-gitlab.test.ts` cases stay with OR-64 and are not ported yet:

- `creates SDKs with legacy default instance URL, token env, headers, and feature flags`
- `uses GITLAB_INSTANCE_URL when instanceUrl is not configured`
- `keeps configured instance URL, apiKey, aiGatewayHeaders, and featureFlags over env/defaults`
- `ignores non-GitLab SDK packages`
- `uses workflowChat for duo workflow models and preserves selectedModelRef`
- `uses exact static workflow model ids when the provider recognizes them`
- `uses provider feature flags instead of model settings feature flags`
- `uses agenticChat with provider aiGatewayHeaders and feature flags for normal models`

The trailing `aisdk.runLanguage` assertion of `discovers workflow models for the location and exposes their refs in the model registry` is the same `language` hook and stays with OR-64; its discovery and model-registry assertions are ported.

Accepted: pending owner
