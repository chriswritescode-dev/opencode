# OR-46: config observers spawn their catalog reload instead of awaiting it

TypeScript: the provider config plugin passes `ctx.integration.reload().pipe(Effect.andThen(ctx.provider.reload()))`
as the observer's reload (`packages/core/src/config/plugin/provider.ts:21-25` at the pinned commit
`73a0eae88de47f10068a0f9dc7a743638fa3d830`), and the policy plugin passes
`State.batch(Effect.all([provider, mcp, skill, websearch reloads], { discard: true }))`
(`packages/core/src/config/plugin/policy.ts:23-28`). `ConfigEntryObserver.observe` awaits that reload inside the
subscription handler for each `config.updated` event, so one update finishes its reload before the next update is
handled; the observer setup itself returns after forking the subscription
(`packages/core/src/config/plugin/entry-observer.ts:16-23`).

Rust: both builtin observers spawn the reload and return. `config_provider` spawns one task that reloads integrations
and then providers (`crates/catalog/src/builtin/config_provider.rs:63-74`), and `config_policy` spawns only
`provider.reload()` (`crates/catalog/src/builtin/config_policy.rs:19-27`). The MCP, skill and web-search reloads the
TypeScript policy plugin also performs are not ported here.

Reason: the config observer callback is synchronous — `opencode_config::observer::observe` takes
`impl Fn(&[Entry]) + Send + 'static` (`crates/config/src/observer.rs:33-54`) — so it cannot await an asynchronous
reload, and spawning keeps the callback returning promptly. Because each update's reload runs detached, a later
config update can start its reload before the earlier one finishes, and `provider.updated`/`model.updated` can be
published after the config write response, so a read immediately after the write can see the previous generation.
`catalog-fixture` writes its models catalog to `OPENCODE_MODELS_PATH` before the server starts and otherwise only
reads provider and model routes, while `catalog-routes` only reads after boot, so no scenario races the observer.

Accepted: pending owner
