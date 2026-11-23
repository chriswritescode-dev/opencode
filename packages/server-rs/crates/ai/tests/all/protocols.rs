use std::sync::Arc;

use opencode_ai::ExecutionContext;
use opencode_ai::MapEnv;
use opencode_ai::clock::FixedClock;
use opencode_ai::id::SequenceIds;
use opencode_ai::route::compile::{StreamOptions, compile};
use opencode_ai::route::model::{Model, ModelDescriptor, ProtocolRegistry};

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
