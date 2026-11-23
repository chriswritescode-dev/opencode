# OR-39: a port-in-use message names no owning process

TypeScript: a managed service that cannot bind within the election window fails with
`Managed service port ${port} on ${hostname} is already in use by ${owner ?? "another process"}. ...`, where `owner`
comes from `PortOwner.describe(port, hostname)` (`packages/cli/src/server-process.ts:184-190`). That helper finds the
listening PID with `lsof` and its command with `ps`, and formats `PID <pid> (<command>)`; on Windows it returns
`undefined`, leaving the generic wording (`packages/cli/src/services/port-owner.ts:10-29`).

Rust: `crates/server/src/service_mode.rs` `elect` returns `Election::PortInUse` after the 15-second window, and `run`
prints `Managed service port <port> on <hostname> is already in use by another process. ...` without looking at the
listening process.

Reason: the listener lookup is not ported and the server has no `lsof`/`ps` wrapper. Covered by
`a_held_port_fails_after_the_election_window` in `crates/server/tests/all/service_mode.rs`, which is `#[ignore]`
because it waits out the full election window.

Accepted: pending owner
