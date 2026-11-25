# OR-50: form validation patterns and date-time parsing

TypeScript: `packages/core/src/form.ts` validates field definitions and answers with JavaScript regular expressions and
`Date`. `new RegExp(field.pattern)` follows ECMAScript regex syntax, the `email` format is a Unicode-aware `\s` pattern,
`URL.canParse` decides `uri`, `isDate` round-trips through `toISOString`, and `isDateTime` is `new Date(value).getTime()`
not being `NaN`, so it accepts the whole of the host's `Date.parse` grammar. Event publication goes through `Bus.Service`,
which can fail, and the service registers an Effect finalizer for `close`.

Rust: `crates/permission/src/form.rs` ports the same validation functions and the same in-memory service over an
`IndexMap` keyed by form id, one `tokio::sync::oneshot` waiter per pending form, a 10 minute retention of settled
entries measured by an injected `FormClock`, and a local `FormState` enum.

Known differences that remain inside Phase 12:

- **`pattern` uses `regex_lite`, a Rust regex subset.** JavaScript-only constructs such as lookahead, lookbehind,
  backreferences and the `d`/`v` flag groups do not compile, so those patterns report
  `Form field has invalid pattern: <key>` where JavaScript would compile and test them. Conversely, some patterns
  JavaScript rejects do compile in `regex_lite`, so the field validates here where upstream reports an invalid pattern:
  inline-flag groups such as `(?i)abc` and named groups written `(?P<name>a)`. Patterns shared by both engines mostly
  behave the same, including unanchored search semantics.
- **Character classes and escapes differ between the engines.** `\d`, `\s` and `\w` are ASCII-only in `regex_lite`, while
  JavaScript `\s` is Unicode-aware, so a user pattern or the `email` format treats non-ASCII whitespace differently: a
  non-breaking space can satisfy the email format here where JavaScript rejects it. `.` matches one Unicode scalar
  here but one UTF-16 code unit in JavaScript without the `u` flag, so an astral character such as an emoji satisfies
  `^.$` here and fails in JavaScript. Constructs that are valid in both engines but mean different things also differ:
  `\A` and `\z` are anchors here but literal `A`/`z` in JavaScript, `[[:alpha:]]` is a POSIX class here but a plain
  bracket class in JavaScript, and `a{,3}` may be a repetition here but is a literal in JavaScript.
- **`isDateTime` implements an ISO subset instead of JavaScript `Date.parse`.** It accepts full-string `YYYY`, `YYYY-MM`
  and `YYYY-MM-DD` dates, and `YYYY-MM-DD[Tt ]HH:MM[:SS[.fraction]][Z|z|±HH:MM|±HHMM]` date-times, with the same
  rollover and hour-24 rules JavaScript applies to those shapes. Legacy and heuristic formats JavaScript also accepts
  (`December 25, 1995`, leading or trailing junk, whitespace padding, expanded signed years) are rejected here, as are
  shorter dates followed by a time such as `2024T10:00` and `2024-01T10:00`, which bun's `Date` accepts.
- **`FormState` is a local enum.** The generated `FormStateAnswered` inlines an unusable `FormValue1` variant type, so
  the service defines `FormState::{Pending, Answered(FormAnswer), Cancelled(Option<String>)}` instead.
- **Event publication is infallible.** `FormEvents::created`, `replied` and `cancelled` return no error, so `create` has
  no rollback after a publish failure and `reply` cannot fail on a publish error. The concrete bus and its error
  handling arrive with the OR-63 composition, which must decide whether a failed publication is logged or surfaced.
- **`close` is an explicit method instead of an Effect finalizer.** Callers must call `FormService::close`; `close` marks
  the service closed, cancels every pending form and resolves their waiters, and is idempotent. A `create` after close
  still inserts, publishes `created`, then immediately cancels, matching upstream behaviour.

Accepted: pending owner
