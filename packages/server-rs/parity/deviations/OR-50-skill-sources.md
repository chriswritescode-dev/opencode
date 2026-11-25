# OR-50: skill sources, watches and report diagnostics

TypeScript: `ConfigSkillPlugin` in `packages/core/src/config/plugin/skill.ts` treats each `skills` entry that parses as an `http:`/`https:` URL as a `Skill.Source` of type `url` and loads it through `SkillDiscovery.pull`. Its watcher subscribes recursively to every source directory (with `SkillScan.ignore`), follows symlinked roots and missing ancestors, and coalesces config-change bursts with a 100 ms debounce. The built-in report skill reads `os.release()` and `process.env` directly.

Rust: `crates/registry/src/config/skill.rs` skips URL entries and loads only directory sources (each `Entry::Directory` contributes `<dir>/skill` and `<dir>/skills`, then each document `skills` entry is expanded against home or the location directory). URL discovery and its cache are deferred to Phase 8. `crates/registry/src/config/compatibility.rs` loads `<root>/skills` for the configured `.claude` and `.agents` roots and reloads on compatibility config changes.

Known differences that remain inside Phase 7:

- **Watch coverage.** Both config loaders use `crate::watch::watch_files` over the currently scanned skill files and source roots. Editing an existing `SKILL.md` reloads, but a skill directory or file created later under an existing source root is not observed until a config update or reconfigure rebuilds the watch set. The TypeScript watcher subscribes recursively to the source directory, so it also sees new skills immediately. The end-to-end test writes the file before registration and then edits it.
- **Report diagnostics.** `builtin/skill.rs` reads the kernel release from `/proc/sys/kernel/osrelease` on Linux and reports `unknown` elsewhere; Node's `os.release()` uses `uname`. The report skill's value lines are replaced with `<value>` in the corpus, and a unit test pins the line format, terminal order, shell fallback, sorted plugin list and OS line shape. Version and channel come from an `AppInfo` argument; the server does not compose a skill service into `GlobalServices`/`serve` yet, so `crate::VERSION`/`crate::CHANNEL` reach it only once registry services are wired in a later task.
- **Config-change coalescing.** The compatibility loader refreshes directly on each matching `ConfigService::changes` event instead of applying a 100 ms debounce first. Refreshes are serialized by a mutex, so correctness is unchanged; a burst only costs extra rescans. `watch_files` still debounces its own notifications by 100 ms.
- **Guidance updates.** Only the initial `core/skill-guidance` render and the empty/removed case are ported; changed/removed update rendering is deferred to OR-55 with the instruction-update deviation.
- **String ordering.** Skill file maps, scan output and guidance summaries sort with Rust byte order instead of JavaScript `localeCompare`/UTF-16 order. This is identical for the ASCII ids and paths the corpus covers.

Accepted: pending owner
