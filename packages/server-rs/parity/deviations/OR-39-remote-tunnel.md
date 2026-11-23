# OR-39: a `remote` service route prints a warning instead of starting a tunnel

TypeScript: when the service config has a `remote` route, the server forks `RemoteTunnel.run` for the bound address
and adds the tunnel URL to the server's URL list: `packages/cli/src/server-process.ts:196-212`.

Rust: `crates/server/src/service_mode.rs` reads `config.remote` only to print
`remote access is not supported by opencode-server yet`; it never starts a tunnel and reports only the local URL.
`crates/server/src/service.rs` `Set` also stops the service and applies the `remote` value without the
`RemoteTunnel.ensure` that `packages/cli/src/services/service-config.ts:263-264` runs when the value is `true`.

Reason: the tunnel client and certificate storage belong to a later networking task. `crates/server/tests/all/service_commands.rs`
covers the config commands; no test starts a tunnel.

Accepted: pending owner
