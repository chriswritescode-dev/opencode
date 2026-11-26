# Operation coverage

Implemented: 83 of 178 operations

| Operation | Method | Path | Stability | Status |
| --- | --- | --- | --- | --- |
| `server.info` | GET | `/api/info` | stable | implemented |
| `server.pair` | POST | `/api/pair` | stable | implemented |
| `server.connect` | GET | `/auth/connect/{code}` | stable | implemented |
| `location.get` | GET | `/api/location` | stable | implemented |
| `location.reload` | POST | `/api/location/reload` | stable | implemented |
| `agent.list` | GET | `/api/agent` | stable | implemented |
| `agent.get` | GET | `/api/agent/{agentID}` | stable | implemented |
| `plugin.list` | GET | `/api/plugin` | stable | implemented |
| `plugin.gui` | GET | `/api/plugin/gui` | stable | implemented |
| `plugin.check` | POST | `/api/plugin/check` | stable | implemented |
| `plugin.update` | POST | `/api/plugin/update` | stable | implemented |
| `session.list` | GET | `/api/session` | stable | implemented |
| `session.create` | POST | `/api/session` | stable | implemented |
| `experimental.session.stats` | GET | `/api/experimental/session/stats` | experimental | not implemented (501) |
| `experimental.session.import` | POST | `/api/experimental/session/import` | experimental | not implemented (501) |
| `experimental.session.export` | GET | `/api/experimental/session/{sessionID}/export` | experimental | not implemented (501) |
| `session.active` | GET | `/api/session/active` | stable | implemented |
| `session.get` | GET | `/api/session/{sessionID}` | stable | implemented |
| `session.remove` | DELETE | `/api/session/{sessionID}` | stable | implemented |
| `session.update` | PATCH | `/api/session/{sessionID}` | stable | implemented |
| `session.fork` | POST | `/api/session/{sessionID}/fork` | stable | not implemented (501) |
| `session.switchAgent` | POST | `/api/session/{sessionID}/agent` | stable | implemented |
| `session.switchModel` | POST | `/api/session/{sessionID}/model` | stable | implemented |
| `session.move` | POST | `/api/session/{sessionID}/move` | stable | not implemented (501) |
| `session.prompt` | POST | `/api/session/{sessionID}/prompt` | stable | implemented |
| `session.command` | POST | `/api/session/{sessionID}/command` | stable | not implemented (501) |
| `experimental.session.skill` | POST | `/api/experimental/session/{sessionID}/skill` | experimental | not implemented (501) |
| `session.synthetic` | POST | `/api/session/{sessionID}/synthetic` | stable | not implemented (501) |
| `session.shell` | POST | `/api/session/{sessionID}/shell` | stable | not implemented (501) |
| `session.compact` | POST | `/api/session/{sessionID}/compact` | stable | not implemented (501) |
| `experimental.session.wait` | POST | `/api/experimental/session/{sessionID}/wait` | experimental | implemented |
| `session.revert.stage` | POST | `/api/session/{sessionID}/revert/stage` | stable | not implemented (501) |
| `session.revert.clear` | DELETE | `/api/session/{sessionID}/revert` | stable | not implemented (501) |
| `session.revert.commit` | POST | `/api/session/{sessionID}/revert/commit` | stable | not implemented (501) |
| `session.context` | GET | `/api/session/{sessionID}/context` | stable | not implemented (501) |
| `session.diff` | GET | `/api/session/{sessionID}/diff` | stable | not implemented (501) |
| `session.inbox.list` | GET | `/api/session/{sessionID}/inbox` | stable | implemented |
| `session.inbox.cancel` | DELETE | `/api/session/{sessionID}/inbox/{inboxID}` | stable | not implemented (501) |
| `session.inbox.update` | PATCH | `/api/session/{sessionID}/inbox/{inboxID}` | stable | not implemented (501) |
| `experimental.session.instructions.entry.list` | GET | `/api/experimental/session/{sessionID}/instructions/entries` | experimental | not implemented (501) |
| `experimental.session.instructions.entry.put` | PUT | `/api/experimental/session/{sessionID}/instructions/entries/{key}` | experimental | not implemented (501) |
| `experimental.session.instructions.entry.remove` | DELETE | `/api/experimental/session/{sessionID}/instructions/entries/{key}` | experimental | not implemented (501) |
| `session.generate` | POST | `/api/session/{sessionID}/generate` | stable | not implemented (501) |
| `experimental.session.log` | GET | `/api/experimental/session/{sessionID}/log` | experimental | implemented |
| `session.interrupt` | POST | `/api/session/{sessionID}/interrupt` | stable | not implemented (501) |
| `session.background` | POST | `/api/session/{sessionID}/background` | stable | not implemented (501) |
| `session.message.get` | GET | `/api/session/{sessionID}/message/{messageID}` | stable | not implemented (501) |
| `session.form.list` | GET | `/api/session/{sessionID}/form` | stable | implemented |
| `session.form.create` | POST | `/api/session/{sessionID}/form` | stable | implemented |
| `session.form.get` | GET | `/api/session/{sessionID}/form/{formID}` | stable | implemented |
| `session.form.cancel` | DELETE | `/api/session/{sessionID}/form/{formID}` | stable | implemented |
| `session.form.reply` | POST | `/api/session/{sessionID}/form/{formID}/reply` | stable | implemented |
| `session.environment` | PUT | `/api/session/{sessionID}/environment` | stable | not implemented (501) |
| `session.view` | POST | `/api/session/{sessionID}/view` | stable | not implemented (501) |
| `session.message.list` | GET | `/api/session/{sessionID}/message` | stable | implemented |
| `model.list` | GET | `/api/model` | stable | implemented |
| `model.default` | GET | `/api/model/default` | stable | implemented |
| `experimental.generate.text` | POST | `/api/experimental/generate/text` | experimental | not implemented (501) |
| `experimental.generate.textLegacy` | POST | `/api/experimental/generate` | experimental | not implemented (501) |
| `experimental.generate.image` | POST | `/api/experimental/generate/image` | experimental | not implemented (501) |
| `experimental.generate.video.create` | POST | `/api/experimental/generate/video` | experimental | not implemented (501) |
| `experimental.generate.video.get` | GET | `/api/experimental/generate/video/{id}` | experimental | not implemented (501) |
| `experimental.generate.decisions` | POST | `/api/experimental/generate/decisions` | experimental | not implemented (501) |
| `experimental.voice.models` | GET | `/api/experimental/voice/models` | experimental | not implemented (501) |
| `experimental.voice.session` | POST | `/api/experimental/voice/session` | experimental | not implemented (501) |
| `provider.list` | GET | `/api/provider` | stable | implemented |
| `provider.get` | GET | `/api/provider/{providerID}` | stable | implemented |
| `integration.list` | GET | `/api/integration` | stable | implemented |
| `integration.get` | GET | `/api/integration/{integrationID}` | stable | implemented |
| `experimental.integration.wellknown.add` | POST | `/api/experimental/integration/wellknown` | experimental | implemented |
| `integration.connect.key` | POST | `/api/integration/{integrationID}/connect/key` | stable | implemented |
| `integration.connect.external` | POST | `/api/integration/{integrationID}/connect/external` | stable | implemented |
| `integration.oauth.connect` | POST | `/api/integration/{integrationID}/connect/oauth` | stable | implemented |
| `integration.oauth.status` | GET | `/api/integration/{integrationID}/connect/oauth/{attemptID}` | stable | implemented |
| `integration.oauth.cancel` | DELETE | `/api/integration/{integrationID}/connect/oauth/{attemptID}` | stable | implemented |
| `integration.oauth.complete` | POST | `/api/integration/{integrationID}/connect/oauth/{attemptID}/complete` | stable | implemented |
| `integration.command.connect` | POST | `/api/integration/{integrationID}/connect/command` | stable | implemented |
| `integration.command.status` | GET | `/api/integration/{integrationID}/connect/command/{attemptID}` | stable | implemented |
| `integration.command.cancel` | DELETE | `/api/integration/{integrationID}/connect/command/{attemptID}` | stable | implemented |
| `mcp.list` | GET | `/api/mcp` | stable | not implemented (501) |
| `experimental.mcp.add` | PUT | `/api/experimental/mcp/{server}` | experimental | not implemented (501) |
| `experimental.mcp.remove` | DELETE | `/api/experimental/mcp/{server}` | experimental | not implemented (501) |
| `experimental.mcp.connect` | POST | `/api/experimental/mcp/{server}/connect` | experimental | not implemented (501) |
| `experimental.mcp.disconnect` | POST | `/api/experimental/mcp/{server}/disconnect` | experimental | not implemented (501) |
| `mcp.resource.catalog` | GET | `/api/mcp/resource` | stable | not implemented (501) |
| `experimental.mcp.client.token` | POST | `/api/experimental/client-mcp/token` | experimental | not implemented (501) |
| `experimental.mcp.client` | GET | `/api/experimental/client-mcp` | experimental | not implemented (501) |
| `credential.list` | GET | `/api/credential` | stable | implemented |
| `credential.create` | POST | `/api/credential` | stable | implemented |
| `credential.update` | PATCH | `/api/credential/{credentialID}` | stable | implemented |
| `credential.remove` | DELETE | `/api/credential/{credentialID}` | stable | implemented |
| `credential.activate` | POST | `/api/credential/{credentialID}/activate` | stable | implemented |
| `project.list` | GET | `/api/project` | stable | implemented |
| `project.update` | PATCH | `/api/project/{projectID}` | stable | implemented |
| `form.list` | GET | `/api/form` | stable | implemented |
| `permission.request.list` | GET | `/api/permission/request` | stable | implemented |
| `permission.saved.list` | GET | `/api/permission/saved` | stable | implemented |
| `permission.saved.update` | PATCH | `/api/permission/saved/{id}` | stable | implemented |
| `permission.saved.remove` | DELETE | `/api/permission/saved/{id}` | stable | implemented |
| `session.permission.create` | POST | `/api/session/{sessionID}/permission` | stable | implemented |
| `session.permission.list` | GET | `/api/session/{sessionID}/permission` | stable | implemented |
| `session.permission.get` | GET | `/api/session/{sessionID}/permission/{requestID}` | stable | implemented |
| `session.permission.reply` | POST | `/api/session/{sessionID}/permission/{requestID}/reply` | stable | implemented |
| `fs.read` | GET | `/api/fs/read/*` | stable | not implemented (501) |
| `fs.list` | GET | `/api/fs/list` | stable | not implemented (501) |
| `fs.find` | GET | `/api/fs/find` | stable | not implemented (501) |
| `experimental.fs.write` | POST | `/api/experimental/fs/write` | experimental | not implemented (501) |
| `experimental.fs.watch` | POST | `/api/experimental/fs/watch` | experimental | not implemented (501) |
| `experimental.fs.unwatch` | DELETE | `/api/experimental/fs/watch` | experimental | not implemented (501) |
| `command.list` | GET | `/api/command` | stable | implemented |
| `skill.list` | GET | `/api/skill` | stable | implemented |
| `rpc.call` | POST | `/api/rpc/{rpcID}/{method}` | stable | not implemented (501) |
| `event.subscribe` | GET | `/api/event` | stable | implemented |
| `pty.list` | GET | `/api/pty` | stable | not implemented (501) |
| `pty.create` | POST | `/api/pty` | stable | not implemented (501) |
| `pty.get` | GET | `/api/pty/{ptyID}` | stable | not implemented (501) |
| `pty.update` | PUT | `/api/pty/{ptyID}` | stable | not implemented (501) |
| `pty.remove` | DELETE | `/api/pty/{ptyID}` | stable | not implemented (501) |
| `pty.connect.token` | POST | `/api/pty/{ptyID}/connect-token` | stable | not implemented (501) |
| `pty.connect` | GET | `/api/pty/{ptyID}/connect` | stable | not implemented (501) |
| `experimental.persistentPty.read` | GET | `/api/experimental/session/{sessionID}/terminal/read` | experimental | not implemented (501) |
| `experimental.persistentPty.list` | GET | `/api/experimental/session/{sessionID}/terminal` | experimental | not implemented (501) |
| `experimental.persistentPty.create` | POST | `/api/experimental/session/{sessionID}/terminal` | experimental | not implemented (501) |
| `experimental.persistentPty.shutdown` | POST | `/api/experimental/persistent-pty/shutdown` | experimental | not implemented (501) |
| `experimental.persistentPty.handoff` | POST | `/api/experimental/persistent-pty/handoff` | experimental | not implemented (501) |
| `experimental.persistentPty.get` | GET | `/api/experimental/persistent-pty/{ptyID}` | experimental | not implemented (501) |
| `experimental.persistentPty.update` | PUT | `/api/experimental/persistent-pty/{ptyID}` | experimental | not implemented (501) |
| `experimental.persistentPty.remove` | DELETE | `/api/experimental/persistent-pty/{ptyID}` | experimental | not implemented (501) |
| `experimental.persistentPty.snapshot` | GET | `/api/experimental/persistent-pty/{ptyID}/snapshot` | experimental | not implemented (501) |
| `experimental.persistentPty.connectToken` | POST | `/api/experimental/persistent-pty/{ptyID}/connect-token` | experimental | not implemented (501) |
| `experimental.persistentPty.connect` | GET | `/api/experimental/persistent-pty/{ptyID}/connect` | experimental | not implemented (501) |
| `experimental.media.get` | GET | `/api/experimental/media/{hash}` | experimental | not implemented (501) |
| `experimental.blob.get` | GET | `/api/experimental/blob/{id}` | experimental | not implemented (501) |
| `experimental.automation.create` | POST | `/api/experimental/automation` | experimental | not implemented (501) |
| `experimental.automation.list` | GET | `/api/experimental/automation` | experimental | not implemented (501) |
| `experimental.automation.get` | GET | `/api/experimental/automation/{automationID}` | experimental | not implemented (501) |
| `experimental.automation.delete` | DELETE | `/api/experimental/automation/{automationID}` | experimental | not implemented (501) |
| `experimental.automation.pause` | POST | `/api/experimental/automation/{automationID}/pause` | experimental | not implemented (501) |
| `experimental.automation.resume` | POST | `/api/experimental/automation/{automationID}/resume` | experimental | not implemented (501) |
| `experimental.artifacts.providers` | GET | `/api/experimental/artifacts/providers` | experimental | not implemented (501) |
| `experimental.artifacts.list` | GET | `/api/experimental/artifacts` | experimental | not implemented (501) |
| `experimental.artifacts.create` | POST | `/api/experimental/artifacts` | experimental | not implemented (501) |
| `experimental.artifacts.get` | GET | `/api/experimental/artifacts/{artifactID}` | experimental | not implemented (501) |
| `experimental.artifacts.update` | PATCH | `/api/experimental/artifacts/{artifactID}` | experimental | not implemented (501) |
| `experimental.artifacts.delete` | DELETE | `/api/experimental/artifacts/{artifactID}` | experimental | not implemented (501) |
| `experimental.artifacts.view` | GET | `/api/experimental/artifacts/{artifactID}/view/*` | experimental | not implemented (501) |
| `experimental.job.list` | GET | `/api/experimental/job` | experimental | implemented |
| `experimental.job.cancel` | POST | `/api/experimental/job/{id}/cancel` | experimental | not implemented (501) |
| `experimental.machine.link` | GET | `/api/experimental/machine/link` | experimental | not implemented (501) |
| `experimental.machine.list` | GET | `/api/experimental/machine` | experimental | not implemented (501) |
| `experimental.machine.add` | POST | `/api/experimental/machine` | experimental | not implemented (501) |
| `experimental.machine.remove` | DELETE | `/api/experimental/machine/{name}` | experimental | not implemented (501) |
| `shell.list` | GET | `/api/shell` | stable | not implemented (501) |
| `shell.create` | POST | `/api/shell` | stable | not implemented (501) |
| `shell.get` | GET | `/api/shell/{id}` | stable | not implemented (501) |
| `shell.remove` | DELETE | `/api/shell/{id}` | stable | not implemented (501) |
| `shell.output` | GET | `/api/shell/{id}/output` | stable | not implemented (501) |
| `reference.list` | GET | `/api/reference` | stable | implemented |
| `worktree.list` | GET | `/api/worktree` | stable | implemented |
| `worktree.create` | POST | `/api/worktree` | stable | implemented |
| `worktree.remove` | DELETE | `/api/worktree` | stable | implemented |
| `worktree.refresh` | POST | `/api/worktree/refresh` | stable | implemented |
| `vcs.init` | POST | `/api/vcs/init` | stable | implemented |
| `vcs.get` | GET | `/api/vcs` | stable | implemented |
| `vcs.base` | GET | `/api/vcs/base` | stable | implemented |
| `vcs.status` | GET | `/api/vcs/status` | stable | implemented |
| `vcs.branch.list` | GET | `/api/vcs/branch` | stable | implemented |
| `vcs.diff` | GET | `/api/vcs/diff` | stable | implemented |
| `debug.heapDump` | POST | `/api/debug/heap-dump` | stable | implemented |
| `debug.location.list` | GET | `/api/debug/location` | stable | implemented |
| `debug.location.evict` | DELETE | `/api/debug/location` | stable | implemented |
| `experimental.migration.v1.status` | GET | `/api/experimental/migration/v1` | experimental | not implemented (501) |
| `websearch.providers` | GET | `/api/websearch/provider` | stable | not implemented (501) |
| `websearch.query` | POST | `/api/websearch` | stable | not implemented (501) |
| `experimental.usage.limits` | GET | `/api/experimental/usage/limits` | experimental | implemented |
| `config.get` | GET | `/api/config` | stable | implemented |
| `config.shells` | GET | `/api/config/shell` | stable | implemented |
| `experimental.config.update` | PATCH | `/api/experimental/config` | experimental | implemented |
