# OR-49: an empty title on session.update is ignored instead of generating a title

TypeScript: `packages/server/src/handlers/session.ts:238-249` treats a present `title` with an empty string as a
request to generate one. A non-empty title renames the session through `session.rename`; an empty title calls
`SessionTitle.Service.generate` with the session's instances, so the server derives a title from the conversation and
publishes a rename.

Rust: `crates/session/src/session.rs:167` only publishes `session.renamed` for a non-empty title, so
`crates/http/src/handlers/session.rs` answers `204 No Content` for `session.update` with `title: ""` and leaves the
title unchanged.

Reason: model-driven title generation needs the model and generation layers and belongs to OR-55. Until then the Rust
server accepts the request without changing the title, which is the closest behavior that needs no model call.

Accepted: pending owner
