# OR-51: the session step executes tools without upstream's cooperative stop and live metadata

TypeScript: `packages/core/src/session/runner/step.ts` starts each local tool call in a scoped fiber, lets the step
publish its result as soon as the tool settles, asks a running tool to stop through its own `cancel` deferred, waits
up to five seconds for that stop, and merges the latest `session.tool.progress` metadata into a failure snapshot.

Rust: `crates/session/src/runner.rs` runs each local call in its own task and publishes success or failure after the
provider stream ends, in call order. On interruption it aborts the tasks and settles every unsettled call with
`aborted` / `Tool execution interrupted`; the cooperative stop window, stale-call recovery and hooks stay with OR-54
and OR-63. Progress metadata is published, but a failure snapshot does not merge it. `crates/session/src/publish_llm_event.rs`
also counts the 2048-character tool input excerpt in Unicode scalars instead of UTF-16 code units.

Reason: the durability and event shape are the same, and the difference is limited to event ordering and the stop
window that the later tasks own. `crates/session/tests/all/runner.rs` covers a tool call followed by text, an unknown
tool, malformed input, a denied permission and an interruption while a tool runs.

Accepted: pending owner

```parity
```
