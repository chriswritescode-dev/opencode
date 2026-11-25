# OR-50: instruction update rendering is deferred to OR-55

TypeScript: `Instructions.Source` in `packages/core/src/instructions/index.ts` carries `initial`, `changed` and `removed` renderers, and `instruction-discovery.ts` renders granular updates with `renderUpdate`, `diffByKey` and `createPatch`. A reload can therefore produce messages such as `The instructions from <path> changed. Here's the diff:` and `Previously loaded instructions no longer apply.` The `core/date` and `core/environment` sources also render `Today's date is now:` and `The environment you are running in is now:` on change.

Rust: `crates/registry/src/instructions/mod.rs` ports only the initialization path: `Key`, `canonical_json`, `hash`, `Observed`, `Source::{key, read, initial}`, `combine` and `render_initial`. `crates/registry/src/instructions/discovery.rs` exposes the full `Instructions from: <path>\n<content>` initial text and no update renderer. `crates/registry/src/config/instruction.rs` rescans and reloads discovery, so consumers see the new initial value, but no granular change diff or removed text is produced.

Reason: the plan places instruction epochs and update rendering in OR-55. The corpus therefore covers discovery order, initial content, the initial `core/instructions` text, the `core/environment` text and the canonical JSON/hash vectors; it has no changed/removed cases. The URL fixture is a Rust test against a local hyper server instead of a corpus case.

Other structure differences that do not change initial behaviour:

- `InstructionEnvironment` (`directory`, `project_directory`, `home`, `config_directory`) is assembled by the caller; the plugin does not resolve `Location` and `Global` services itself.
- `builtins::Environment.vcs` is the VCS kind string (`"git"` or `"hg"`), not the typed `Location` VCS value.
- The config plugin rebuilds the whole watch set after each refresh instead of subscribing and unsubscribing individual candidates; the watched candidate set matches the plugin's.
- An absolute configured glob without a literal parent starts from the deepest existing literal directory and scans the remaining pattern, where the TypeScript code passes the absolute pattern to `Glob.scan`.

Accepted: pending owner
