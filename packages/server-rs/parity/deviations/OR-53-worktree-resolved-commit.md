# OR-53: a resolved worktree row and its durable `worktree.resolved` event commit separately

TypeScript: `packages/core/src/project.ts:141` (`persist`) inserts a directory's `worktree` row and publishes the
durable `Worktree.Event.Resolved` (`packages/core/src/project.ts:228`) in the same transaction, so a recorded worktree
always has its event and the session projector moves the adopted sessions.

Rust: `ProjectService::persist` in `crates/location/src/project.rs:354` computes the directory's adopted projects
before inserting its `worktree` row, then sends `LocationEvent::WorktreeResolved` on the process-local broadcast
channel after the row commits. `publish_location_event` in `crates/http/src/services.rs:543` publishes the durable
`worktree.resolved` from that channel later. A directory whose adoption or insert fails keeps no row, so the next
resolve retries it; but a lagged subscriber or a process exit between the commit and the publication leaves a row
without a durable `worktree.resolved`, and the session projector never moves its sessions.

Reason: the Location layer records rows through `opencode-storage` and does not depend on `opencode-bus`, so the event
is bridged after the commit instead of inside it. Publishing in the same commit and backfilling rows recorded without
an event needs the bus inside the project transaction. No parity scenario covers the failure window.

Accepted: pending owner
