# OR-47: provider sign-in, token and JWKS requests are bounded in time

TypeScript: the ChatGPT ID-token check fetches the JWKS through `jose`'s `createRemoteJWKSet` (`packages/core/src/plugin/provider/chatgpt.ts:504`, `:509`), whose `timeoutDuration` defaults to 5000 ms (`jose@6.0.11` `dist/webapi/jwks/remote.js`). The token endpoints of ChatGPT (`chatgpt.ts:525`), GitHub Copilot's device-code request, poll and user lookup (`github-copilot.ts:493`), and GitLab's code exchange and refresh (`gitlab.ts:415`) use a bare `fetch` or the unbounded `HttpClient.HttpClient`, with only the caller's interrupt signal.

Rust: `crates/catalog/src/provider_plugin/chatgpt.rs` gives the JWKS fetch 5 seconds, which matches the `jose` default, and token requests 10 seconds. `crates/catalog/src/provider_plugin/github_copilot/oauth.rs` gives the device-code request, every poll and the login user lookup 10 seconds. `crates/catalog/src/provider_plugin/gitlab.rs` gives the token exchange and refresh 10 seconds. These reuse the same 10 second bound as the Azure and DigitalOcean discovery calls. A timed-out JWKS fetch fails ID-token verification as an invalid token; a timed-out sign-in or refresh request fails with a request error.

Reason: a credential refresh holds `credential-refresh:<id>` across processes, and the GitLab refresh also holds `refresh_gate::run`'s process-wide single flight, so an unbounded token request blocks every other refresher of the same connection. GitHub Copilot's device flow would otherwise wait forever on a stalled poll. The 10 second bound is the only difference from the TypeScript client, which relies on the caller's interrupt to end a stalled request.

Accepted: pending owner
