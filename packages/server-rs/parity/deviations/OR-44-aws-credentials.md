# OR-44: Bedrock uses only configured credentials, not the AWS default provider chain

TypeScript: `packages/ai/src/protocols/utils/bedrock-auth.ts:29-53` exports `defaultChain`, which imports `fromNodeProviderChain` and calls it with `{ ignoreCache: true, profile }` on every request. The chain reads environment variables, shared config and SSO caches, web identity tokens, process credentials, and container or instance metadata, and a failure becomes an `Authentication` error (pinned commit `73a0eae88de47f10068a0f9dc7a743638fa3d830`).

Rust: `crates/ai/src/protocols/utils/bedrock_auth.rs` `resolve_auth` only accepts an explicit `api_key` (or `AWS_BEARER_TOKEN_BEDROCK`) and a static `Credentials`; when neither is present it returns `Authentication` with "the default credential chain is not available". Its `sigv4` builds an `Auth::SigV4` value from supplied credentials. Neither function is wired into request construction: `crates/ai/src/route/model.rs` `Model::from_descriptor` copies the descriptor's `auth` straight into the route and never calls `resolve_auth`, and `crates/ai/src/route/auth.rs` signs only with the credentials already in that `Auth`. The only `resolve_auth` callers are in `crates/ai/tests/all/sigv4.rs`.

Reason: the default provider chain needs the AWS SDK, and OR-46 owns credentials. Until it lands, a Bedrock route must receive bearer auth or static credentials explicitly; an unconfigured route fails with `Authentication` where TypeScript discovers credentials from the environment or instance metadata.

Accepted: pending owner
