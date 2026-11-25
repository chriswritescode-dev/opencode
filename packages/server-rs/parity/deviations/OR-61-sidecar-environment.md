# OR-61: plugins run in a sidecar with an allowlisted environment

TypeScript: plugin modules are imported into the server process (`packages/core/src/plugin/module.ts:174`,
`Host.load`) and run against the in-process host (`packages/core/src/plugin/host.ts:53`), so they read the server's
whole `process.env` and see its working directory.

Rust: the sidecar starts Bun in a private temporary directory with `env_clear` and only the `NAMES`, `LC_*`,
`XDG_*` and `NPM_CONFIG_*` allowlist from `crates/sidecar/src/config.rs`; the prefix is compared upper-case, so
`npm_config_registry` passes, and `NAMES` also carries `NODE_EXTRA_CA_CERTS` because npm registry, authentication
and CA settings reach the sidecar through `process.env` (`packages/util/src/npm-config.ts:19`) and plugin installs
need them. `OPENCODE_PASSWORD` and `OPENCODE_SERVER_PASSWORD` never reach it, and it runs with `--no-env-file` and
`--no-install` (`crates/sidecar/src/spawn.rs`).

Reason: OR-61 runs the JavaScript in its own process so a plugin crash or leak cannot take down the server, and gives
it only what it needs. The check is `crates/sidecar/tests/all/spawn.rs`
(`the_host_sees_only_the_allowlisted_environment`).

Accepted: pending owner
