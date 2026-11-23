# OR-39: malformed arguments print one fixed usage line and exit with code 1

TypeScript: the CLI uses an Effect `Command` run through `Command.run` (`packages/cli/src/framework/runtime.ts:84-90`),
with the root defined at `packages/cli/src/commands/commands.ts:72-80`; Effect CLI prints its own usage/help text
and suggestions.

Rust: `crates/server/src/cli.rs:3` defines one `usage: opencode-server <serve|service|--version>` line; malformed
arguments print it, and unknown commands/flags print `unknown command: <name>` / `unknown flag: <name>`. Both exit
with code 1.

Reason: the Rust server has only the `serve` and `service` command trees and does not use Effect CLI.
`an_unknown_command_names_it_and_exits_one`, `an_unknown_serve_flag_names_it_and_exits_one`,
`an_out_of_range_port_exits_one` and `no_arguments_prints_usage_and_exits_one` in
`crates/server/tests/all/version.rs` cover the exit code and message, `service_and_stdio_together_exit_one` in
`crates/server/tests/all/serve.rs` covers a fixed-line message, and `crates/server/tests/all/service_commands.rs`
covers an unknown `service get` key.

Accepted: pending owner
