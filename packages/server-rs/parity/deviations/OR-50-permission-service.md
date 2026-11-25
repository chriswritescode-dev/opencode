# OR-50: permission service seams and failure shapes

TypeScript: `packages/core/src/permission.ts` owns `Permission.Service`. It reads the session through
`SessionStore.Service`, resolves agents through `Agent.Service`, stores approvals through
`PermissionSaved.Service`, publishes `permission.asked` and `permission.replied` through `Bus.Service`, and calls the
`permission` `evaluate` plugin hook through `PluginHooks.Service`. `Unattended` is an Effect context reference. A
closed service dies with `Permission.DeclinedError` on `assert`, a duplicate pending id dies, and a failed bus publish
fails `create` after removing the pending request.

Rust: `crates/permission/src/service.rs` owns the same pending store (`IndexMap` in insertion order, one
`tokio::sync::oneshot` per request), evaluation order (`configured` deny, saved rules, per-resource ask promotion,
evaluate hook, unattended settlement), `ask`/`assert`/`assertNotDenied`/`allows`/`reply`/`get`/`forSession`/`list`/
`nested`/`close`, and the `Always` path that saves and re-evaluates the remaining requests. Sessions, agents, events
and the evaluate hook arrive as the traits `PermissionSessions`, `PermissionAgents`, `PermissionEvents` and
`EvaluateHook`; `PermissionSaved` and the project id are injected. `NoEvaluate` is the default hook.

Known differences that remain inside Phase 11:

- **Event publication is infallible.** `PermissionEvents::asked` and `replied` return no error, so `create` has no
  post-publish cleanup and `reply` cannot fail on a publish error. The concrete bus and its error handling arrive with
  the OR-63 composition, which must decide whether a failed publication is logged or surfaced.
- **Closed and duplicate outcomes are errors, not defects.** `assert` on a closed service returns
  `PermissionError::Declined`, a duplicate pending id returns `PermissionError::Duplicate`, and saved-rule or storage
  failures return `PermissionError::Saved`. TypeScript dies instead; the plan requires `Declined` as a distinct
  variant that callers treat as a defect.
- **`reply` is not uninterruptible as a whole.** Two concurrent `reply` calls for the same request can both succeed;
  TypeScript serializes them on one fiber and the second sees `NotFound`. Normal single-client use is unaffected.
- **`Unattended` is an explicit input field.** `AssertInput.unattended` carries the settlement rules instead of an
  Effect context reference.
- **Evaluate hook shape.** The hook receives a local `PermissionEvaluation` type and returns the same type; no plugin
  hook registry is wired yet. OR-63 supplies the sidecar implementation that produces the result.
- **Dependency wiring.** The composer implements `PermissionSessions` over `SessionStore::get` and `PermissionAgents`
  over `AgentService::resolve`; the service itself has no dependency on `opencode-session` or `opencode-registry`.
- **Tests.** The ported service tests use fake sessions, fake agents and a recording event sink. Saved approvals use
  the real `PermissionSaved` on an in-memory database, because the storage layer is the unit under test for `Always`.

Accepted: pending owner
