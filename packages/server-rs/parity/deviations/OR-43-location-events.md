# OR-43: project, worktree and location events are not published on the event feed

TypeScript: `packages/core/src/project.ts:170`, `:227`, `:283`, `:297` and `:501` publish `project.updated` and
`worktree.resolved` on the bus, and `packages/core/src/location-lifecycle.ts:66` publishes `location.shutdown` when a
Location's services shut down, so `/api/event` subscribers see project changes and can recover after
`location.reload` or an eviction.

Rust: `opencode-location` sends the same facts as `LocationEvent` on a process-local broadcast channel
(`ProjectService::publish` in `crates/location/src/project.rs` and `Lifecycle::shutdown` in
`crates/location/src/lifecycle.rs`), and `crates/http/src/services.rs` exposes it through
`GlobalServices::subscribe_events`, but nothing forwards it to `opencode-bus`, so the event feed carries none of these
events.

Reason: OR-43 owns the Location layer, and bridging its channel to the bus is left to the event-feed work. The OR-43
scenarios run without `waitEvent` steps and are compared with `--only steps,database`, so no parity run covers these
events yet; the strict run due in OR-49 does.

Since OR-53, `ProjectService::persist` writes the `worktree` rows in the project transaction and sends
`worktree.resolved` on the channel only for directories it newly inserted, while TypeScript commits the row and the
durable event together. Until the bridge lands, a directory first resolved by the Rust server keeps its row but has no
durable `worktree.resolved`, so the session projector never moves its sessions; the bridge must publish the event in
the same commit and backfill rows recorded without one.

Accepted: pending owner

```parity
* events/*
```
