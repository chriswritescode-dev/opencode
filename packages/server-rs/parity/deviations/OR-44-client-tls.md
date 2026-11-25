# OR-44: per-request client certificates are not presented

TypeScript: `packages/ai/src/route/transport/http.ts:88` sends every provider request through `RequestExecutor.withTls(runtime.http.execute, request.http?.tls)`, and `packages/ai/src/route/executor.ts:306-310` provides the `http.tls` client certificate from `packages/ai/src/schema/options.ts:76` to the executor's HTTP client for that request (pinned commit `e54fbacb3d3aebc85f0e5ada0a5c490cc9333a91`).

Rust: `crates/ai/src/schema/options.rs` decodes and merges `http.tls`, but `crates/ai/src/route/executor.rs` `ReqwestSender` builds one shared client whose `tls_config` uses `with_no_client_auth`, and neither `crates/ai/src/route/transport/http.rs` `json_post` nor `crates/ai/src/route/client.rs` reads `http.tls`. A request that sets `http.tls` is sent without a client certificate.

Reason: no corpus cassette sets `http.tls`, so `crates/ai/tests/all/replay.rs` does not observe it; presenting a certificate needs a per-certificate client in `ReqwestSender`, which belongs with the provider configuration in OR-46. A provider that requires mutual TLS fails its handshake where TypeScript succeeds.

Accepted: pending owner
