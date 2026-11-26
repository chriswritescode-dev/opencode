# OR-48: artifact titles and event descriptions count Unicode scalars instead of UTF-16 units

TypeScript: the artifact title is cut with `input.title.slice(0, 200).trim()`
(`packages/core/src/plugin/provider/opencode-artifacts.ts:68`) and an event type's description with
`.slice(0, MAX_DESCRIPTION)` (`packages/core/src/plugin/provider/opencode-watches.ts:381`). A JavaScript string index
counts UTF-16 code units, so an emoji or another non-BMP character counts as two and can be cut between its surrogate
halves.

Rust: `artifacts.rs:159` uses `chars().take(200)` and `watches/tools.rs:250` uses `chars().take(MAX_DESCRIPTION)`. A Rust
`char` is one Unicode scalar, so the same text is cut later and never split. The artifact slug and ID suffix are built
from ASCII only, so their 47- and 54-character limits are unaffected.

Reason: Rust strings are UTF-8 and iterate by scalar value, and splitting a surrogate pair is not representable. The
difference only appears when a title or description contains non-BMP characters near the limit; the ported artifact and
event-list tests use ASCII text.

Accepted: pending owner
