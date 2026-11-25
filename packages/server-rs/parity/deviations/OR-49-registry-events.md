# OR-49: plugin and catalog registries do not publish their updated events

TypeScript: the plugin and catalog registries publish `integration.updated`, `provider.updated`, `model.updated`,
`agent.updated`, `command.updated`, `skill.updated`, `artifact.updated`, `websearch.updated`, `reference.updated`
and `plugin.updated` when a Location loads and when its configuration changes, for example
`packages/core/src/integration.ts:364`, `provider.ts:505`, `model.ts:273`, `agent.ts:90`, `command.ts:70`,
`skill.ts:159`, `artifact.ts:263`, `websearch.ts:122`, `reference.ts:107` and `plugin.ts:48` at the pinned commit.

Rust: the Rust server has no integration, provider, model, agent, command, skill, artifact, websearch, reference or
plugin registry yet, so it publishes none of these events on the event feed. The registries arrive with OR-46, OR-50
and OR-62.

Reason: there is no registry to notify until those tasks land. `parity/scenarios/location-routes.scenario.ts` and
`parity/scenarios/config-routes.scenario.ts` load and reload Locations and would observe these events.

Accepted: pending owner

```parity
location-routes events/*
config-routes events/*
```
