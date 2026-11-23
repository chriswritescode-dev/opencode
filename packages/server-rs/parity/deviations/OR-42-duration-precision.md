# OR-42: durations beyond i128 nanoseconds or with 39 or more fractional digits are rejected

TypeScript: Effect 4.1.0 `Duration` parses a duration string with `parseNanos` and `fromMillisString` over `BigInt`
nanoseconds (`effect/src/Duration.ts` in the pinned dependency version), so any magnitude and any number of
fractional digits decodes. `Schema.DurationFromString` reaches it for `experimental.location_idle_timeout`
(`packages/schema/src/config/experimental.ts:62` at the pinned commit
`73a0eae88de47f10068a0f9dc7a743638fa3d830`), `warming.interval` (`packages/schema/src/config/warming.ts:10`) and
`warming.duration` (`:16`).

Rust: `crates/config/src/normalize.rs` `parse_nanos` (`:1491-1509`) and `from_millis_string` (`:1526-1538`) use
`i128` nanoseconds with `checked_*` arithmetic and `pow10`. A value outside the `i128` range, or with 39 or more
fractional digits, makes them return `None`, so `duration_string` fails and the value is reported with an `invalid`
diagnostic and dropped, for `location_idle_timeout` (`:733-748`) and for the `warming` fields (`:870-895`).

Reason: exact parity would need a big-integer representation for no practical gain, and no realistic configuration
gives these fields a value anywhere near that size.
