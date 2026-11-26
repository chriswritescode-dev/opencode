# OR-49: some registries do not publish their updated events

TypeScript: the plugin and catalog registries publish `integration.updated`, `provider.updated`, `model.updated`,
`agent.updated`, `command.updated`, `skill.updated`, `artifact.updated`, `websearch.updated`, `reference.updated`
and `plugin.updated` when a Location loads and when its configuration changes, for example
`packages/core/src/integration.ts:364`, `provider.ts:505`, `model.ts:273`, `agent.ts:90`, `command.ts:70`,
`skill.ts:159`, `artifact.ts:263`, `websearch.ts:122`, `reference.ts:107` and `plugin.ts:48` at the pinned commit.

Rust: the agent, command, skill, reference and plugin registries publish their events, so those types appear on the
event feed. The integration, provider, model, artifact and websearch registries do not exist in the Rust server yet
and publish nothing. The event feed of a Location load therefore has a different set, count and order of events, and
a positional event comparison sees the later types shifted.

Reason: there is no registry to notify until the remaining integration, provider, model, artifact and websearch
registries land. `parity/scenarios/location-routes.scenario.ts` and `parity/scenarios/config-routes.scenario.ts`
load and reload Locations and observe these events.

Accepted: pending owner

```parity
location-routes events/**
config-routes events/**
```
