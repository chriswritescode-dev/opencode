# OR-49: prompt attachments and skills are rejected instead of materialized

TypeScript: `packages/server/src/handlers/session.ts:289-326` passes `files`, `agents` and `skills` from the
`session.prompt` request into `Session.prompt`, and `SessionPrompt.prepare`
(`packages/core/src/session/prompt.ts:34`) resolves the attachments before admission. `materializeAttachment`
(`packages/core/src/session/prompt.ts:94`) turns each file attachment into an inline `data:` or stored `media:` part,
and the skill branch (`packages/core/src/session/prompt.ts:58-80`) looks each `Skill.ID` up in the skill registry and
prepares the skill text. A missing skill fails with `Session.SkillNotFoundError`, which the handler answers as
`InvalidRequestError` with field `skills`. A skill that fails to load now fails with `Skill.LoadError`, which the
prompt handler maps through `skillLoadFailed`.

Rust: the `prompt` handler in `crates/http/src/handlers/session.rs` answers `501 Not Implemented` with an
`UnknownError` body whose message is `opencode-server does not implement prompt attachments yet` when `files` or
`skills` is non-empty. `agents` are accepted and carried into the inbox payload.

Reason: file materialization needs the file tools from OR-51 and skill resolution needs the skill registry from
OR-50. Until then the server refuses the request instead of silently dropping the attachments and skills a client
asked for.

Accepted: pending owner
