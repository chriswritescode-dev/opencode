# OR-44: Bedrock uses only configured credentials, not the AWS default provider chain

TypeScript: `packages/ai/src/protocols/utils/bedrock-auth.ts:29-53` exports `defaultChain`, which imports `fromNodeProviderChain` and calls it with `{ ignoreCache: true, profile }` on every request. The chain reads environment variables, shared config and SSO caches, web identity tokens, process credentials, and container or instance metadata, and a failure becomes an `Authentication` error (pinned commit `e54fbacb3d3aebc85f0e5ada0a5c490cc9333a91`).

Rust: `crates/ai/src/protocols/utils/bedrock_auth.rs` `resolve_auth` only accepts an explicit `api_key` (or `AWS_BEARER_TOKEN_BEDROCK`) and a static `Credentials`; when neither is present it returns `Authentication` with "the default credential chain is not available". Its `sigv4` builds an `Auth::SigV4` value from supplied credentials. Both `crates/ai/src/providers/amazon_bedrock.rs` and `crates/ai/src/providers/amazon_bedrock_mantle.rs` call `resolve_auth` during model construction, so a configured Bedrock route uses bearer auth or its supplied static credentials. Only the default provider chain itself is unavailable until OR-46.

Reason: the default provider chain needs the AWS SDK, and OR-46 owns credentials. Until it lands, a Bedrock route must receive bearer auth or static credentials explicitly; an unconfigured route fails with `Authentication` where TypeScript discovers credentials from the environment or instance metadata.

Accepted: pending owner
