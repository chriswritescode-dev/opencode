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
