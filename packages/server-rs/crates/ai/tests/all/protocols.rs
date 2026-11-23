use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures_util::{StreamExt, stream};
use opencode_ai::ExecutionContext;
use opencode_ai::MapEnv;
use opencode_ai::clock::FixedClock;
use opencode_ai::id::SequenceIds;
use opencode_ai::protocols::openai_compatible_responses;
use opencode_ai::protocols::utils::{responses_checkpoint, responses_compaction};
use opencode_ai::route::auth::Auth;
use opencode_ai::route::client::LlmClient;
use opencode_ai::route::compile::{StreamOptions, compile};
use opencode_ai::route::endpoint::PathPart;
use opencode_ai::route::executor::{BoxFuture, ByteStream, HttpResponse, HttpSender, PreparedHttp, RequestExecutor};
use opencode_ai::route::model::{Model, ModelDescriptor, ProtocolRegistry, RouteTransport};
use opencode_ai::schema::{AiError, AiErrorReason, FinishReason, LlmEvent, LlmRequest, StringMap};
use serde_json::{Value, json};

#[test]
fn openai_chat_prepared_bodies_match_corpus() {
    let corpus = crate::corpus::Corpus::load();
    let cassettes = corpus
        .cassettes
        .iter()
        .filter(|cassette| cassette.group == "openai-chat")
        .collect::<Vec<&crate::corpus::Cassette>>();
    assert_eq!(cassettes.len(), 11, "openai-chat cassette count");
    let steps = cassettes.iter().map(|cassette| cassette.steps.len()).sum::<usize>();
    assert_eq!(steps, 15, "openai-chat step count");
    let registry = ProtocolRegistry::builtin();
    let mut checked = 0usize;
    for cassette in cassettes {
        for (index, step) in cassette.steps.iter().enumerate() {
            let descriptor: ModelDescriptor = serde_json::from_value(step.model.clone())
                .unwrap_or_else(|error| panic!("{} step {index}: {error}", cassette.name));
            let model = Model::from_descriptor(descriptor, &registry)
                .unwrap_or_else(|error| panic!("{} step {index}: {}", cassette.name, error.message()));
            let context = ExecutionContext::new(
                Arc::new(FixedClock::new(cassette.clock_epoch_ms)),
                Arc::new(SequenceIds::new()),
                Arc::new(MapEnv::default()),
            );
            let compiled = compile(&model, step.request(), &StreamOptions::default(), &context)
                .unwrap_or_else(|error| panic!("{} step {index}: {}", cassette.name, error.message()));
            let exchange = step
                .exchanges
                .as_ref()
                .and_then(|exchanges| exchanges.first())
                .unwrap_or_else(|| panic!("{} step {index}: no recorded exchange", cassette.name));
            let body = String::from_utf8(compiled.prepared.request.body.to_vec())
                .unwrap_or_else(|error| panic!("{} step {index}: {error}", cassette.name));
            assert_eq!(body, exchange.prepared.body, "{} step {index} prepared.body", cassette.name);
            assert_eq!(
                compiled.prepared.request.url, exchange.prepared.url,
                "{} step {index} prepared.url",
                cassette.name
            );
            assert_eq!(
                compiled.prepared.request.method, exchange.prepared.method,
                "{} step {index} prepared.method",
                cassette.name
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 15, "openai-chat steps checked");
}

#[test]
fn anthropic_body_headers_cover_every_beta() {
    let corpus = crate::corpus::Corpus::load();
    let cassette = corpus.cassettes.iter().find(|cassette| cassette.group == "anthropic-messages").unwrap();
    let descriptor: ModelDescriptor = serde_json::from_value(cassette.steps[0].model.clone()).unwrap();
    let model = Model::from_descriptor(descriptor, &ProtocolRegistry::builtin()).unwrap();
    let protocol = model.route.protocol.clone();
    let empty: LlmRequest = serde_json::from_value(json!({"system": [], "messages": [], "tools": []})).unwrap();
    assert_eq!(
        protocol.body_headers(&model, &empty, &json!({"messages": []})),
        vec![("anthropic-beta".to_string(), "interleaved-thinking-2025-05-14".to_string())]
    );
    let mut request = empty;
    request.headers = Some(StringMap::from_pairs([(
        "anthropic-beta".to_string(),
        "custom-beta,interleaved-thinking-2025-05-14".to_string(),
    )]));
    let body = json!({
        "messages": [
            {"role": "system", "content": [{"type": "tool_addition"}], "output_config": {"effort": "high"}},
            {"role": "assistant", "content": [{"type": "compaction"}]}
        ],
        "context_management": {"edits": [{}]},
        "speed": "fast",
        "thinking": {"type": "adaptive", "block_binding": {"prefix_mismatch_behavior": "drop_block"}}
    });
    assert_eq!(
        protocol.body_headers(&model, &request, &body),
        vec![(
            "anthropic-beta".to_string(),
            concat!(
                "custom-beta,interleaved-thinking-2025-05-14,compact-2026-01-12,",
                "mid-conversation-output-config-2026-07-01,inline-tools-2026-09-15,",
                "fast-mode-2026-02-01,thinking-binding-controls-2026-08-01"
            )
            .to_string()
        )]
    );
}

#[test]
fn anthropic_effort_updates_follow_model_version() {
    let corpus = crate::corpus::Corpus::load();
    let cassette = corpus
        .cassettes
        .iter()
        .find(|cassette| cassette.name == "anthropic-messages/anthropic-opus-4-7-image-tool-result")
        .unwrap();
    let registry = ProtocolRegistry::builtin();
    let mut descriptor = cassette.steps[0].model.clone();
    let request: LlmRequest = serde_json::from_value(json!({"system": [], "messages": [], "tools": []})).unwrap();
    let model = Model::from_descriptor(serde_json::from_value(descriptor.clone()).unwrap(), &registry).unwrap();
    assert!(!model.route.protocol.supports_effort_updates(&model, &request));
    descriptor["id"] = json!("claude-opus-5");
    let model = Model::from_descriptor(serde_json::from_value(descriptor).unwrap(), &registry).unwrap();
    assert!(model.route.protocol.supports_effort_updates(&model, &request));
    let disabled: LlmRequest = serde_json::from_value(json!({
        "system": [],
        "messages": [],
        "tools": [],
        "providerOptions": {"thinking": {"type": "disabled"}}
    }))
    .unwrap();
    assert!(!model.route.protocol.supports_effort_updates(&model, &disabled));
}

fn corpus_step(name: &str, index: usize) -> (Model, LlmRequest) {
    let corpus = crate::corpus::Corpus::load();
    let cassette = corpus.cassettes.iter().find(|cassette| cassette.name == name).unwrap();
    let step = cassette.steps[index].clone();
    let descriptor: ModelDescriptor = serde_json::from_value(step.model.clone()).unwrap();
    let model = Model::from_descriptor(descriptor, &ProtocolRegistry::builtin()).unwrap();
    (model, step.request())
}

fn with_http_body(request: &LlmRequest, body: Value) -> LlmRequest {
    let mut request = request.clone();
    request.http.get_or_insert_with(Default::default).body = Some(body);
    request
}

#[test]
fn checkpoint_request_appends_a_compaction_trigger() {
    let (model, request) = corpus_step("openai-responses/openai-responses-gpt-5-5-reasoning", 0);
    let checkpoint = responses_checkpoint::checkpoint_request(&request, &model).unwrap();
    let input = checkpoint.body["input"].as_array().unwrap();
    assert!(input.len() > 1, "input keeps the canonical history");
    assert_eq!(input.last().unwrap(), &json!({"type": "compaction_trigger"}));
    assert_eq!(checkpoint.body["model"], json!(model.id));
    assert_eq!(checkpoint.body["stream"], json!(true));
    assert!(checkpoint.url.ends_with("/responses"), "{}", checkpoint.url);

    let streamed = with_http_body(&request, json!({"stream": true}));
    assert_eq!(responses_checkpoint::checkpoint_request(&streamed, &model).unwrap().body["stream"], json!(true));
}

#[test]
fn checkpoint_request_rejects_history_and_stream_overrides() {
    let (model, request) = corpus_step("openai-responses/openai-responses-gpt-5-5-reasoning", 0);
    let history = "Trigger compaction requires complete canonical history, not an input or continuation override";
    for body in [json!({"input": []}), json!({"previous_response_id": null}), json!({"conversation": []})] {
        let error = responses_checkpoint::checkpoint_request(&with_http_body(&request, body), &model).unwrap_err();
        assert_eq!(error.message(), history);
    }
    let error = responses_checkpoint::checkpoint_request(&with_http_body(&request, json!({"stream": false})), &model)
        .unwrap_err();
    assert_eq!(error.message(), "Trigger compaction requires a streamed response");
}

#[test]
fn compact_request_targets_the_compact_path_and_drops_streaming() {
    let (model, request) = corpus_step("openai-responses/openai-responses-gpt-5-5-reasoning", 0);
    let compact = responses_compaction::compact_request(&request, &model).unwrap();
    assert!(compact.url.ends_with("/responses/compact"), "{}", compact.url);
    assert_eq!(compact.body["model"], json!(model.id));
    assert!(compact.body["input"].is_array());
    assert!(compact.body.get("stream").is_none());
    assert!(compact.body.get("max_output_tokens").is_none());
    assert_eq!(compact.body["reasoning"], json!({"effort": "low", "summary": "auto"}));
}

#[test]
fn compact_request_declares_tools_at_the_top_level() {
    let (model, request) = corpus_step("openai-responses-images/generates-and-edits-an-image-with-the-hosted-tool", 0);
    let compact = responses_compaction::compact_request(&request, &model).unwrap();
    assert!(compact.url.ends_with("/responses/compact"), "{}", compact.url);
    let tools = compact.body["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["type"], json!("image_generation"));
}

#[test]
fn openai_compatible_responses_route_reuses_open_responses() {
    let route = openai_compatible_responses::route();
    assert_eq!(route.id, "openai-compatible-responses");
    assert_eq!(route.provider_metadata_key.as_deref(), Some("openresponses"));
    assert_eq!(route.protocol.id(), "open-responses");
    match &route.endpoint.path {
        PathPart::Static(path) => assert_eq!(path, "/responses"),
        _ => panic!("openai-compatible-responses endpoint is not a static path"),
    }
    assert!(route.endpoint.base_url.is_none());
    assert_eq!(route.transport, RouteTransport::Http);
    assert!(matches!(route.auth, Auth::None));
    assert!(route.headers.is_none());
    let provider_options = route.options.as_ref().unwrap().provider_options.as_ref().unwrap();
    assert_eq!(provider_options.get("store"), Some(&json!(false)));
    assert_eq!(provider_options.get("include"), Some(&json!(["reasoning.encrypted_content"])));
}

struct ContinuationSender {
    responses: Mutex<VecDeque<Vec<u8>>>,
    requests: Mutex<Vec<PreparedHttp>>,
}

impl HttpSender for ContinuationSender {
    fn send(&self, request: PreparedHttp) -> BoxFuture<Result<HttpResponse, AiError>> {
        self.requests.lock().expect("continuation requests").push(request);
        let body = self.responses.lock().expect("continuation responses").pop_front().expect("scripted response");
        let body: ByteStream = Box::pin(stream::once(async move { Ok(Bytes::from(body)) }));
        Box::pin(async move { Ok(HttpResponse::new(200, Vec::new(), body)) })
    }
}

#[tokio::test]
async fn gemini_continuation_resends_the_body_with_the_token() {
    let descriptor: ModelDescriptor = serde_json::from_value(json!({
        "id": "gemini-3-pro",
        "provider": "google",
        "route": {"id": "gemini", "protocol": "gemini", "providerMetadataKey": "google"},
        "endpoint": {"baseURL": "https://generativelanguage.googleapis.com/v1beta", "path": null},
        "transport": "http",
        "auth": {"type": "header", "name": "x-goog-api-key", "value": "fixture"}
    }))
    .unwrap();
    let model = Model::from_descriptor(descriptor, &ProtocolRegistry::builtin()).unwrap();
    let request: LlmRequest = serde_json::from_value(json!({
        "system": [],
        "messages": [{"role": "user", "content": [{"type": "text", "text": "Say hi."}]}],
        "tools": [],
        "maxTokens": 16,
        "temperature": 0,
        "http": {"body": {"labels": {"a": "b"}}}
    }))
    .unwrap();
    let first = concat!(
        "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"Hel\"}]},",
        "\"finishReason\":\"CONTINUATION\",\"continuationToken\":\"tok\"}]}\n\n"
    );
    let second = concat!(
        "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"lo\"}]},",
        "\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":1,",
        "\"candidatesTokenCount\":1,\"totalTokenCount\":2}}\n\n"
    );
    let sender = Arc::new(ContinuationSender {
        responses: Mutex::new(VecDeque::from([first.as_bytes().to_vec(), second.as_bytes().to_vec()])),
        requests: Mutex::new(Vec::new()),
    });
    let clock = Arc::new(FixedClock::new(1_767_323_045_000));
    let context = ExecutionContext::new(clock.clone(), Arc::new(SequenceIds::new()), Arc::new(MapEnv::default()));
    let client = LlmClient::new(RequestExecutor::new(sender.clone(), None, clock), context);
    let items =
        client.stream(&model, request, StreamOptions::default()).collect::<Vec<Result<LlmEvent, AiError>>>().await;
    let events = items
        .into_iter()
        .map(|item| item.unwrap_or_else(|error| panic!("stream failed: {error}")))
        .collect::<Vec<LlmEvent>>();
    let requests = sender.requests.lock().expect("continuation requests");
    assert_eq!(requests.len(), 2, "both exchanges were sent");
    assert!(sender.responses.lock().expect("continuation responses").is_empty(), "both responses were consumed");
    let body: Value = serde_json::from_slice(&requests[1].body).expect("second request body");
    assert_eq!(body["continuationToken"], json!("tok"));
    let first: Value = serde_json::from_slice(&requests[0].body).expect("first request body");
    assert_eq!(first["labels"], json!({"a": "b"}), "the first request carries the http.body overlay");
    assert_eq!(body["labels"], json!({"a": "b"}), "the continuation keeps the http.body overlay");
    let length = requests[1]
        .headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .map(|(_, value)| value.clone())
        .expect("second request content-length");
    assert_eq!(length, requests[1].body.len().to_string());
    let deltas = events
        .iter()
        .filter_map(|event| match event {
            LlmEvent::TextDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<&str>>();
    assert_eq!(deltas, vec!["Hel", "lo"]);
    match events.last() {
        Some(LlmEvent::Finish { reason, .. }) => {
            assert_eq!(reason.normalized, FinishReason::Stop);
            assert_eq!(reason.raw.as_deref(), Some("STOP"));
        }
        other => panic!("the stream did not finish: {other:?}"),
    }
}

#[tokio::test]
async fn gemini_non_json_frame_reports_the_route_id() {
    let descriptor: ModelDescriptor = serde_json::from_value(json!({
        "id": "gemini-3-pro",
        "provider": "google",
        "route": {"id": "gemini", "protocol": "gemini", "providerMetadataKey": "google"},
        "endpoint": {"baseURL": "https://generativelanguage.googleapis.com/v1beta", "path": null},
        "transport": "http",
        "auth": {"type": "header", "name": "x-goog-api-key", "value": "fixture"}
    }))
    .unwrap();
    let model = Model::from_descriptor(descriptor, &ProtocolRegistry::builtin()).unwrap();
    let request: LlmRequest = serde_json::from_value(json!({
        "system": [],
        "messages": [{"role": "user", "content": [{"type": "text", "text": "Say hi."}]}],
        "tools": []
    }))
    .unwrap();
    let sender = Arc::new(ContinuationSender {
        responses: Mutex::new(VecDeque::from([b"data: not-json\n\n".to_vec()])),
        requests: Mutex::new(Vec::new()),
    });
    let clock = Arc::new(FixedClock::new(1_767_323_045_000));
    let context = ExecutionContext::new(clock.clone(), Arc::new(SequenceIds::new()), Arc::new(MapEnv::default()));
    let client = LlmClient::new(RequestExecutor::new(sender, None, clock), context);
    let items =
        client.stream(&model, request, StreamOptions::default()).collect::<Vec<Result<LlmEvent, AiError>>>().await;
    match items.last() {
        Some(Err(error)) => match &error.reason {
            AiErrorReason::InvalidProviderOutput { message, body, route, .. } => {
                assert_eq!(route.as_deref(), Some("gemini"));
                assert_eq!(message, "Invalid google/gemini stream event");
                assert_eq!(body.as_deref(), Some("not-json"));
            }
            other => panic!("expected invalid provider output, got {other:?}"),
        },
        other => panic!("expected the stream to fail, got {other:?}"),
    }
}
