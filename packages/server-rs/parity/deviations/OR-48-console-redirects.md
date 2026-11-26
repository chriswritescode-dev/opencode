# OR-48: a redirected Console request keeps its content type when rewritten to a GET

TypeScript: `redirected` rewrites a 303, or a 301/302 after a POST, to `HttpClientRequest.make("GET")` with
`body: HttpBody.empty` (`packages/core/src/plugin/provider/opencode.ts:745-760`). The empty body replaces the previous
body and its content headers, so the redirected GET carries no `content-type`.

Rust: the same rewrite is in `crates/catalog/src/provider_plugin/opencode/follow.rs:76-86`; it empties the body but
keeps every header except the credential headers when the origin changes, so the rewritten GET still sends the original
`content-type`. Other redirect details match: the hop limit is five, 301/302/303/307/308 with a `location` are followed,
the redirect body is drained and a read failure is ignored, and the final answer must be 2xx.

Reason: the Rust follower rewrites a `PreparedHttp` value, which has no body-derived header model, so it cannot tell a
`content-type` set for the old body from one the caller set deliberately. The ported tests cover the 302-to-GET body,
the hop limit and the cross-origin credential drop but not the leftover header.

Accepted: pending owner
