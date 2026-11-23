use std::sync::Arc;

use opencode_ai::ExecutionContext;
use opencode_ai::MapEnv;
use opencode_ai::clock::FixedClock;
use opencode_ai::id::SequenceIds;
use opencode_ai::route::compile::{StreamOptions, compile};
use opencode_ai::route::model::{Model, ModelDescriptor, ProtocolRegistry};
use opencode_ai::schema::{LlmRequest, StringMap};
use serde_json::json;

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
