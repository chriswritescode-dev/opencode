# OR-47: ChatGPT token requests and JWKS fetches are bounded in time

TypeScript: `verifyIDToken` fetches the JWKS through `jose`'s `createRemoteJWKSet` (`packages/core/src/plugin/provider/chatgpt.ts:504`, `:509`), whose `timeoutDuration` defaults to 5000 ms (`jose@6.0.11` `dist/webapi/jwks/remote.js`). The token endpoint calls use a bare `fetch` with only the interrupt signal (`packages/core/src/plugin/provider/chatgpt.ts:525`), so a stalled token endpoint hangs the exchange or refresh until the caller is interrupted.

Rust: `crates/catalog/src/provider_plugin/chatgpt.rs` gives the JWKS fetch a 5 second timeout, which matches the `jose` default, and the token endpoint a 10 second timeout, the same bound the Azure and DigitalOcean discovery calls use. A timed-out JWKS fetch fails ID-token verification as an invalid token; a timed-out token request fails the sign-in or refresh with a request error.

Reason: a credential refresh holds `credential-refresh:<id>` across processes, so an unbounded token request blocks other processes from refreshing the same connection. The 10 second bound is the only difference from the TypeScript client, which relies on the caller's interrupt to end a stalled request.

Accepted: pending owner
