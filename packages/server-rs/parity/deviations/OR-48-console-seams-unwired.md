# OR-48: Console content, watches, search and artifacts are not registered with the Rust services yet

TypeScript: the plugin publishes Console content through the core registries. `syncInstructionsAndSkills` registers
the workspace's instructions with `InstructionDiscovery` and reloads `Skill.Service` after a download
(`packages/core/src/plugin/provider/opencode.ts:367-384`). It offers hosted web search through
`ctx.websearch.transform` (`:511`), the four `console_watch` tools through `ctx.tool.transform` (`opencode-watches.ts:258`),
and reloads the tool registry after every watch sync (`opencode-watches.ts:426`). The `console` artifact provider is
added through `ctx.experimental.artifact.transform` and is the default while the header is present
(`opencode-artifacts.ts:35`).

Rust: `crates/catalog/src/provider_plugin/opencode/handle.rs` exposes the same data through `ConsoleHandle`: `instructions()`
(`:172`), `skills()` (`:176`), `web_search()` (`:104`), `watch_tools()` (`:253`) and `artifacts()` (`:125`), and
`sync_instructions_and_skills` plus `sync_artifacts` keep them current (`opencode/mod.rs:576-616`, `handle.rs:149-166`).
No Rust caller reads them outside the plugin and its tests: there is no instruction or skill registry to fill (OR-50), no
web-search registry (OR-58), no tool registration or reload (OR-54) and no artifact registry (OR-59). The watch sync also
has no `ctx.tool.reload()` equivalent (`watches/mod.rs:34`). The 401/403 explainer that `guard` registers through
`ProviderHooks::on_http_response` runs only when a caller triggers `trigger_http_response`, which no Rust session does
yet (`OR-47-provider-hooks-unwired`).

Reason: those registries belong to OR-50, OR-54, OR-57, OR-58 and OR-59. This task ports the Console client and proves it
against `ConsoleHandle` directly, so the server serves none of the Console instructions, skills, watch tools, hosted search
or artifacts until those tasks register the seam. The ported plugin tests exercise `ConsoleHandle` and are the coverage.

Accepted: pending owner
