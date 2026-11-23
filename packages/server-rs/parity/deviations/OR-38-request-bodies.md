# OR-38: request bodies decode into the wire representation

TypeScript: a JSON request body is decoded through the endpoint payload schema's own codec, and
`packages/schema/src/schema.ts:36-40` defines `DateTimeUtcFromMillis` as `Schema.Finite` decoded into
`Schema.DateTimeUtc`. The wire therefore carries epoch milliseconds. The type-side rendering used for the string
slots (`Schema.toCodecJson(Schema.toType(slot))`) is not the wire form: it renders those fields as ISO-8601 strings,
for example `session.view`'s `idle` input `9` renders as `"1970-01-01T00:00:00.009Z"`.

Rust: `crates/contract/src/generated/requests.rs` generates request body types from the encoded (wire) schema, the
same way the response and event types are generated, and `crates/contract/src/request.rs` `json_body` decodes the
wire JSON. `DateTimeUtc` fields stay `f64` epoch milliseconds. `crates/contract/tests/all/requests.rs` therefore
compares each body against the TypeScript wire decode re-encoded through the same wire codec; string slots
(`params`, `query`, `headers`) still compare against the type-side render.

Parameters, query strings and headers are transformed into the typed JSON form before decoding: numbers and
booleans are parsed from their string forms, `null` is recognised for nullable fields, and a repeated key becomes
the single array element list that `Schema.toCodecArrayFromSingle` produces. String-slot fields keep the type-side
render.

Reason: a Rust server needs the wire values it can forward without re-encoding them, and reproducing the type-side
`DateTimeUtc` rendering would require ISO-8601 formatting for the full JavaScript date range. The request corpus
covers the wire form for every operation; `tests/all/requests.rs` and `tests/all/coverage.rs` cover the split.

Accepted: pending owner
