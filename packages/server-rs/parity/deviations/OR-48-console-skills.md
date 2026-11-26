# OR-48: a Console skill list is cleared synchronously instead of by the download fiber

TypeScript: `syncInstructionsAndSkills` always runs `loadSkills` in a `FiberHandle`
(`packages/core/src/plugin/provider/opencode.ts:367-384`). An empty list or a disallowed document is still a fiber, so
the skills already shown stay until the fiber runs and replaces them; a newer list replaces an in-flight download.

Rust: `sync_instructions_and_skills` (`crates/catalog/src/provider_plugin/opencode/mod.rs:576-616`) clears the skills
immediately when the document is not allowed, the connection is gone or the list is empty, and otherwise aborts the
current download and starts a new one. The instructions are set synchronously in both.

Reason: the Rust download is a `JoinHandle` that is aborted and replaced, and an empty list has nothing to download, so
there is no fiber to defer the empty result. The visible difference is limited to the instant between a config change
and the next skill read; the ported tests cover edit, Console error, 404, a malformed list and the stale-download race.

Accepted: pending owner
