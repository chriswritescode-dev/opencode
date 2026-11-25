# OR-47: loopback OAuth callbacks reject a missing or mismatched state before anything else

TypeScript: the OpenAI and ChatGPT loopback handlers read `error`/`error_description` and the missing authorization code before comparing `state` (`packages/core/src/plugin/provider/openai.ts:77`, `:86`, `packages/core/src/plugin/provider/chatgpt.ts:129`, `:138`), so any request to the callback URL aborts the pending sign-in. DigitalOcean's `/auth/token` handler does the same for `error` and a missing access token (`packages/core/src/plugin/provider/digitalocean.ts:70`, `:72`). The Poe handler already checks `state` first (`packages/core/src/plugin/provider/poe.ts:131`), and the GitLab and Snowflake Cortex handlers follow the same order.

Rust: `crates/catalog/src/provider_plugin/openai.rs` (`handle`), `crates/catalog/src/provider_plugin/chatgpt.rs` (`handle`) and `crates/catalog/src/provider_plugin/digitalocean.rs` (`handle`) compare `state` first and, on a mismatch, answer 400 without resolving the pending attempt. A matching-state provider error, and a missing code or access token after a matching state, still fail the attempt with the TypeScript messages.

Reason: a stray local request to the callback URL, such as a browser prefetch, a health check, or another local process, could otherwise cancel a sign-in that the user is still completing. A real provider error response carries the `state` it was given, so checking it first does not change the outcome of a genuine denial. A malformed DigitalOcean token body has no state, so it is now ignored instead of failing the attempt.

Accepted: pending owner
