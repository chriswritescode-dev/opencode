# Re-sync 1

## Drift report

- from: `73a0eae88de47f10068a0f9dc7a743638fa3d830`
- to: `e54fbacb3d3aebc85f0e5ada0a5c490cc9333a91`

## Operations

10 added, 0 removed, 106 changed

### Added

- `experimental.skill.file` `GET /api/experimental/skill/{skillID}/file/*`
- `experimental.push.get` `GET /api/experimental/push`
- `experimental.push.device.register` `PUT /api/experimental/push/device/{deviceID}`
- `experimental.push.device.remove` `DELETE /api/experimental/push/device/{deviceID}`
- `experimental.push.device.update` `PATCH /api/experimental/push/device/{deviceID}`
- `experimental.push.device.session.set` `PUT /api/experimental/push/device/{deviceID}/session/{sessionID}`
- `experimental.push.device.session.remove` `DELETE /api/experimental/push/device/{deviceID}/session/{sessionID}`
- `experimental.push.session.set` `PUT /api/experimental/push/session/{sessionID}`
- `experimental.push.session.remove` `DELETE /api/experimental/push/session/{sessionID}`
- `experimental.push.settings.set` `PUT /api/experimental/push/settings`

### Changed

- `location.get` `GET /api/location`
- `agent.list` `GET /api/agent`
- `agent.get` `GET /api/agent/{agentID}`
- `plugin.list` `GET /api/plugin`
- `plugin.gui` `GET /api/plugin/gui`
- `plugin.check` `POST /api/plugin/check`
- `plugin.update` `POST /api/plugin/update`
- `session.list` `GET /api/session`
- `session.create` `POST /api/session`
- `experimental.session.stats` `GET /api/experimental/session/stats`
- `experimental.session.export` `GET /api/experimental/session/{sessionID}/export`
- `session.active` `GET /api/session/active`
- `session.fork` `POST /api/session/{sessionID}/fork`
- `session.switchAgent` `POST /api/session/{sessionID}/agent`
- `session.switchModel` `POST /api/session/{sessionID}/model`
- `session.prompt` `POST /api/session/{sessionID}/prompt`
- `session.command` `POST /api/session/{sessionID}/command`
- `experimental.session.skill` `POST /api/experimental/session/{sessionID}/skill`
- `session.synthetic` `POST /api/session/{sessionID}/synthetic`
- `session.compact` `POST /api/session/{sessionID}/compact`
- `experimental.session.wait` `POST /api/experimental/session/{sessionID}/wait`
- `session.diff` `GET /api/session/{sessionID}/diff`
- `session.inbox.list` `GET /api/session/{sessionID}/inbox`
- `experimental.session.log` `GET /api/experimental/session/{sessionID}/log`
- `session.interrupt` `POST /api/session/{sessionID}/interrupt`
- `session.message.get` `GET /api/session/{sessionID}/message/{messageID}`
- `session.form.cancel` `DELETE /api/session/{sessionID}/form/{formID}`
- `session.message.list` `GET /api/session/{sessionID}/message`
- `model.list` `GET /api/model`
- `model.default` `GET /api/model/default`
- `experimental.generate.video.create` `POST /api/experimental/generate/video`
- `experimental.generate.video.get` `GET /api/experimental/generate/video/{id}`
- `provider.list` `GET /api/provider`
- `provider.get` `GET /api/provider/{providerID}`
- `integration.list` `GET /api/integration`
- `integration.get` `GET /api/integration/{integrationID}`
- `experimental.integration.wellknown.add` `POST /api/experimental/integration/wellknown`
- `integration.connect.key` `POST /api/integration/{integrationID}/connect/key`
- `integration.connect.external` `POST /api/integration/{integrationID}/connect/external`
- `integration.oauth.connect` `POST /api/integration/{integrationID}/connect/oauth`
- `integration.oauth.status` `GET /api/integration/{integrationID}/connect/oauth/{attemptID}`
- `integration.oauth.cancel` `DELETE /api/integration/{integrationID}/connect/oauth/{attemptID}`
- `integration.oauth.complete` `POST /api/integration/{integrationID}/connect/oauth/{attemptID}/complete`
- `integration.command.connect` `POST /api/integration/{integrationID}/connect/command`
- `integration.command.status` `GET /api/integration/{integrationID}/connect/command/{attemptID}`
- `integration.command.cancel` `DELETE /api/integration/{integrationID}/connect/command/{attemptID}`
- `mcp.list` `GET /api/mcp`
- `experimental.mcp.add` `PUT /api/experimental/mcp/{server}`
- `experimental.mcp.remove` `DELETE /api/experimental/mcp/{server}`
- `experimental.mcp.connect` `POST /api/experimental/mcp/{server}/connect`
- `experimental.mcp.disconnect` `POST /api/experimental/mcp/{server}/disconnect`
- `mcp.resource.catalog` `GET /api/mcp/resource`
- `experimental.mcp.client.token` `POST /api/experimental/client-mcp/token`
- `experimental.mcp.client` `GET /api/experimental/client-mcp`
- `project.list` `GET /api/project`
- `form.list` `GET /api/form`
- `permission.request.list` `GET /api/permission/request`
- `permission.saved.list` `GET /api/permission/saved`
- `fs.read` `GET /api/fs/read/*`
- `fs.list` `GET /api/fs/list`
- `fs.find` `GET /api/fs/find`
- `experimental.fs.write` `POST /api/experimental/fs/write`
- `experimental.fs.watch` `POST /api/experimental/fs/watch`
- `experimental.fs.unwatch` `DELETE /api/experimental/fs/watch`
- `command.list` `GET /api/command`
- `skill.list` `GET /api/skill`
- `rpc.call` `POST /api/rpc/{rpcID}/{method}`
- `event.subscribe` `GET /api/event`
- `pty.list` `GET /api/pty`
- `pty.create` `POST /api/pty`
- `pty.get` `GET /api/pty/{ptyID}`
- `pty.update` `PUT /api/pty/{ptyID}`
- `pty.remove` `DELETE /api/pty/{ptyID}`
- `pty.connect.token` `POST /api/pty/{ptyID}/connect-token`
- `pty.connect` `GET /api/pty/{ptyID}/connect`
- `experimental.persistentPty.read` `GET /api/experimental/session/{sessionID}/terminal/read`
- `experimental.persistentPty.connectToken` `POST /api/experimental/persistent-pty/{ptyID}/connect-token`
- `experimental.persistentPty.connect` `GET /api/experimental/persistent-pty/{ptyID}/connect`
- `experimental.blob.get` `GET /api/experimental/blob/{id}`
- `experimental.automation.list` `GET /api/experimental/automation`
- `experimental.artifacts.providers` `GET /api/experimental/artifacts/providers`
- `experimental.artifacts.list` `GET /api/experimental/artifacts`
- `experimental.artifacts.create` `POST /api/experimental/artifacts`
- `experimental.artifacts.get` `GET /api/experimental/artifacts/{artifactID}`
- `experimental.artifacts.update` `PATCH /api/experimental/artifacts/{artifactID}`
- `experimental.artifacts.delete` `DELETE /api/experimental/artifacts/{artifactID}`
- `experimental.artifacts.view` `GET /api/experimental/artifacts/{artifactID}/view/*`
- `experimental.job.list` `GET /api/experimental/job`
- `shell.list` `GET /api/shell`
- `shell.create` `POST /api/shell`
- `shell.get` `GET /api/shell/{id}`
- `shell.remove` `DELETE /api/shell/{id}`
- `shell.output` `GET /api/shell/{id}/output`
- `reference.list` `GET /api/reference`
- `vcs.init` `POST /api/vcs/init`
- `vcs.get` `GET /api/vcs`
- `vcs.base` `GET /api/vcs/base`
- `vcs.status` `GET /api/vcs/status`
- `vcs.branch.list` `GET /api/vcs/branch`
- `vcs.diff` `GET /api/vcs/diff`
- `debug.heapDump` `POST /api/debug/heap-dump`
- `debug.location.evict` `DELETE /api/debug/location`
- `websearch.providers` `GET /api/websearch/provider`
- `websearch.query` `POST /api/websearch`
- `experimental.usage.limits` `GET /api/experimental/usage/limits`
- `config.get` `GET /api/config`

## Component schemas

15 added, 0 removed, 10 changed

### Added

- `Model.Speed`
- `Push.Bell`
- `Push.Device`
- `Push.DeviceID`
- `Push.DevicePatch`
- `Push.DeviceSettings`
- `Push.Info`
- `Push.Level`
- `Push.Platform`
- `Push.Register`
- `Push.Registered`
- `Push.Settings`
- `PushDeviceNotFoundErrorEncoded`
- `SkillLoadErrorEncoded`
- `experimental.push.updated`

### Changed

- `Agent.Info`
- `Automation.CodeActionInput`
- `Automation.Trigger`
- `Config.InfoEncoded`
- `Model.Info`
- `Prompt.AgentAttachment`
- `PromptInput.FileAttachment`
- `Session.Inbox.Delivery`
- `Skill.Info`
- `V2Event`

## Public events

1 added, 0 removed

### Added

- `experimental.push.updated`

## Durable/session-log members

0 added, 0 removed

## Migrations

0 added, 0 removed, 0 modified

## Absorbed

Each change below is ported into `packages/server-rs` at the new pin, with the phase that ported it.

### Phase 1: merge, pin and regeneration

- The contract is regenerated from `e54fbacb3d`: 10 added operations (`experimental.skill.file` and the nine
  `experimental.push.*` operations), 106 changed, the `Push.*` and `SkillLoadError` schemas, and the
  `experimental.push.updated` event. The pinned counts in the contract tests and `docs/coverage.md` moved with it.
- Compile adaptations for the new pin: `Flavor::root` in config's Windows path check, `experimental: None` on a
  catalog provider model, and `instructions: None` on the session's default agent.

### Phase 2: local-only flags and renamed media tools

- `opencode_api` is local-only: the generated table carries `LOCAL_ONLY` from upstream `flags.localOnly`, and
  `flag.rs` resolves a local-only key to disabled with source `default` on non-local/dev channels before any
  environment or config override (upstream `571619bc15`).
- `experimental_image_generate` and `experimental_video_generate` are renamed to `generate_image` and
  `generate_video` in V1 permission normalization by one `renamed_tool` owner that both the action and permission
  paths use (upstream `7b70422787`).

### Phase 3: agent instructions and model speeds

- The Plan agent carries upstream's selected and unselected instruction objects, rendered from
  `<home>/.opencode/plan`, and a `core/agent` instruction source appends the agent's instructions last when the
  agent changes (upstream `30ef864fe1`). Persisted system messages now reach the LLM request, so an instruction
  update in history is sent.
- The catalog disables an enabled speed model whose base model is missing or disabled within the provider, matching
  upstream `model.ts` (upstream `7edab789f7`).

### Phase 4: the stashed session cwd stays out of public metadata

- The projector stores and keeps upstream's `$opencode_experimental_cwd` in the session row's metadata, and
  `Session.Info` reads the metadata without it, so session get and list never expose the stashed cwd (upstream
  `21d5ab15d7`).

### Phase 5: LLM layer

- `Protocol.namespaces` and one `shared::flatten_request` after `prepare` flatten tool namespaces once, before every
  adapter; the per-adapter `flatten_tool_request` hooks are gone (upstream `3b38f3aadd`).
- xAI compaction flattens its request like every step (upstream `d3abc6706c`).
- Freeform tools set their other parameters on an order-free first line with a generated grammar, and their formats
  come from `capabilities.tools.freeform` (upstream `e33e872628` and `553c14178e`).

## Deferred

Upstream changes in areas this server does not implement yet. Each names the task that owns it.

- Session cwd behaviour and its instructions, the runner and step Code Mode changes, and moving subagents with their
  parent → OR-54. The metadata-hiding half of session cwd is absorbed in phase 4.
- The permission `Policy` algebra, MCP-hosted skills, `experimental.skill.file` and `SkillLoadError` → OR-50.
- The freeform tool `field` and the model resolver's freeform copy → OR-51.
- The git `%(symref)` branch filter → OR-53.
- models.dev service-tier speeds and introductory re-pricing → OR-46.
- Environments, workspaces and the `cd` tool → OR-60.
- Code Mode `opencode://` fetch → OR-65.
- Plugin-host and Promise-adapter abort changes → OR-62 and OR-63.
- Blob file cleanup and the `generate_image`/`generate_video` renames outside config normalization → OR-59.
- The nine `experimental.push.*` operations and `experimental.push.updated` → no task yet; the
  `OR-39-not-implemented-operations` deviation answers them with the 501 body.

## Step times

| Phase | Scope | Time |
| --- | --- | --- |
| 1 | Merge, pin, regeneration and compile | 501 s |
| 2 | Local-only flags and media tool renames | 646 s |
| 3 | Agent instructions and model speeds | 651 s |
| 4 | Session cwd metadata | 469 s |
| 5 | LLM layer | 1166 s |
| 6 | Deviation ledger, this record and the full checks | 1065 s |

Phase 1 is the runner's measured time from start to handoff (14:24:29 to 14:32:50 UTC). The phase 2 to 5 times are
the intervals between the loop checkpoint commits `b320667482`, `cb14661f86`, `6fb61c028a`, `5f7190c4ac` and
`8902bc7c6d`. Phase 6 is timed from that last checkpoint to the last check in `## Checks`.
