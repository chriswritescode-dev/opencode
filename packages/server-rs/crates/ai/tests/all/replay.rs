use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use bytes::Bytes;
use futures_util::{StreamExt, stream};
use serde_json::Value;

use opencode_ai::ExecutionContext;
use opencode_ai::MapEnv;
use opencode_ai::clock::FixedClock;
use opencode_ai::id::SequenceIds;
use opencode_ai::modalities::decision::{DecisionClient, DecisionRequestInput};
use opencode_ai::modalities::embedding::{EmbeddingClient, EmbeddingRequestInput};
use opencode_ai::modalities::image::{ImageClient, ImageEvent, ImageRequestInput};
use opencode_ai::providers::{ProviderPackages, RuntimeModel, Settings};
use opencode_ai::route::client::LlmClient;
use opencode_ai::route::compile::StreamOptions;
use opencode_ai::route::executor::{BoxFuture, ByteStream, HttpResponse, HttpSender, PreparedHttp, RequestExecutor};
use opencode_ai::route::model::{Model, ModelDescriptor, ProtocolRegistry};
use opencode_ai::route::transport::channel::{
    ChannelCheckpoint, ChannelExchange, ChannelExecution, ChannelExecutor, ChannelMode, ChannelObservation,
};
use opencode_ai::schema::{AiError, LlmEvent, Recovery};

use crate::corpus::{Cassette, ChannelOp, Exchange, Prepared, Step, WebSocketRecord};

const CHUNKED_BODY_SIZE: usize = 7;

pub async fn replay_cassette(cassette: &Cassette) {
    let registry = ProtocolRegistry::builtin();
    for (index, step) in cassette.steps.iter().enumerate() {
        let model = model_for_step(cassette, index, step, &registry);
        let exchanges = match step.exchanges.as_ref() {
            Some(exchanges) if !exchanges.is_empty() => exchanges.clone(),
            _ => panic!("{} step {index} exchanges: the step recorded no HTTP exchange", cassette.name),
        };
        let recorded_events = match step.events.as_ref() {
            Some(events) => events.clone(),
            None if step.error.is_some() => Vec::new(),
            None => panic!("{} step {index} events: the step recorded no events and no error", cassette.name),
        };
        for chunk in [None, Some(CHUNKED_BODY_SIZE)] {
            let label = match chunk {
                None => "single-chunk".to_string(),
                Some(size) => format!("{size}-byte-chunk"),
            };
            let sender = Arc::new(RecordedSender {
                cassette: cassette.name.clone(),
                step: index,
                label: label.clone(),
                chunk,
                exchanges: Mutex::new(VecDeque::from(exchanges.clone())),
            });
            let clock = Arc::new(FixedClock::new(cassette.clock_epoch_ms));
            let context =
                ExecutionContext::new(clock.clone(), Arc::new(SequenceIds::new()), Arc::new(MapEnv::default()));
            let client = LlmClient::new(RequestExecutor::new(sender.clone(), None, clock), context);
            let items = client
                .stream(&model, step.request(), StreamOptions::default())
                .collect::<Vec<Result<LlmEvent, AiError>>>()
                .await;
            sender.assert_consumed();
            assert_outcome(cassette, index, &label, step.error.as_ref(), &recorded_events, items);
        }
    }
}

fn model_for_step(cassette: &Cassette, index: usize, step: &Step, registry: &ProtocolRegistry) -> Model {
    let Some(provider) = step.provider.as_ref() else {
        let descriptor: ModelDescriptor = serde_json::from_value(step.model.clone())
            .unwrap_or_else(|error| panic!("{} step {index} model descriptor: {error}", cassette.name));
        return Model::from_descriptor(descriptor, registry)
            .unwrap_or_else(|error| panic!("{} step {index} model: {}", cassette.name, error.message()));
    };
    let settings = Settings::new(provider.settings.clone());
    let runtime = ProviderPackages::builtin()
        .model(&provider.package, &provider.model_id, &settings, &MapEnv::default())
        .unwrap_or_else(|error| {
            panic!("{} step {index} provider {}: {}", cassette.name, provider.package, error.message())
        });
    let RuntimeModel::Language(model) = runtime else {
        panic!("{} step {index} provider {}: expected a language model", cassette.name, provider.package);
    };
    let actual = without_auth(
        serde_json::to_value(model.descriptor())
            .unwrap_or_else(|error| panic!("{} step {index} provider descriptor: {error}", cassette.name)),
    );
    let expected = without_auth(step.model.clone());
    assert_eq!(actual, expected, "{} step {index} provider descriptor", cassette.name);
    model
}

fn without_auth(mut descriptor: Value) -> Value {
    if let Some(object) = descriptor.as_object_mut() {
        object.remove("auth");
    }
    descriptor
}

pub async fn replay_group(group: &str) -> usize {
    let corpus = crate::corpus::Corpus::load();
    let mut steps = 0;
    for cassette in corpus.cassettes.iter().filter(|cassette| cassette.group == group) {
        replay_cassette(cassette).await;
        steps += cassette.steps.len();
    }
    steps
}

pub async fn replay_embedding_cassette(cassette: &Cassette) {
    for (index, step) in cassette.steps.iter().enumerate() {
        let Some(provider) = step.provider.as_ref() else {
            panic!("{} step {index} embedding: the step recorded no provider", cassette.name);
        };
        let settings = Settings::new(provider.settings.clone());
        let runtime = ProviderPackages::builtin()
            .model(&provider.package, &provider.model_id, &settings, &MapEnv::default())
            .unwrap_or_else(|error| {
                panic!("{} step {index} provider {}: {}", cassette.name, provider.package, error.message())
            });
        let RuntimeModel::Embedding(model) = runtime else {
            panic!("{} step {index} embedding: expected an embedding model", cassette.name);
        };
        assert_eq!(model.id, step.model["id"], "{} step {index} embedding model.id", cassette.name);
        assert_eq!(model.provider, step.model["provider"], "{} step {index} embedding model.provider", cassette.name);
        assert_eq!(
            model.route.id, step.model["route"]["id"],
            "{} step {index} embedding model.route.id",
            cassette.name
        );
        let input: EmbeddingRequestInput = serde_json::from_value(step.request.clone())
            .unwrap_or_else(|error| panic!("{} step {index} embedding request: {error}", cassette.name));
        let request = input.with_model(model);
        let exchanges = match step.exchanges.as_ref() {
            Some(exchanges) if !exchanges.is_empty() => exchanges.clone(),
            _ => panic!("{} step {index} embedding exchanges: the step recorded no HTTP exchange", cassette.name),
        };
        let sender = Arc::new(RecordedSender {
            cassette: cassette.name.clone(),
            step: index,
            label: "embedding".to_string(),
            chunk: None,
            exchanges: Mutex::new(VecDeque::from(exchanges)),
        });
        let clock = Arc::new(FixedClock::new(cassette.clock_epoch_ms));
        let context = ExecutionContext::new(clock.clone(), Arc::new(SequenceIds::new()), Arc::new(MapEnv::default()));
        let executor = RequestExecutor::new(sender.clone(), None, clock);
        let outcome = EmbeddingClient::generate(request, &executor, &context).await;
        sender.assert_consumed();
        assert_embedding_outcome(cassette, index, step.error.as_ref(), outcome);
    }
}

pub async fn replay_embedding_group(group: &str) -> usize {
    let corpus = crate::corpus::Corpus::load();
    let mut steps = 0;
    for cassette in corpus.cassettes.iter().filter(|cassette| cassette.group == group) {
        replay_embedding_cassette(cassette).await;
        steps += cassette.steps.len();
    }
    steps
}

fn assert_embedding_outcome(
    cassette: &Cassette,
    index: usize,
    recorded_error: Option<&crate::corpus::StepError>,
    outcome: Result<opencode_ai::modalities::embedding::EmbeddingResponse, AiError>,
) {
    match recorded_error {
        Some(recorded) => {
            let error = match outcome {
                Err(error) => error,
                Ok(_) => panic!("{} step {index} embedding error: the generate call succeeded", cassette.name),
            };
            let reason = serde_json::to_value(&error.reason)
                .unwrap_or_else(|failure| panic!("{} step {index} embedding error.reason: {failure}", cassette.name));
            assert_eq!(reason, recorded.reason, "{} step {index} embedding error.reason", cassette.name);
            assert_eq!(
                error.is_retryable(),
                recorded.retryable,
                "{} step {index} embedding error.retryable",
                cassette.name
            );
        }
        None => {
            let response = match outcome {
                Ok(response) => response,
                Err(error) => {
                    panic!("{} step {index} embedding result: the generate call failed with {error}", cassette.name)
                }
            };
            let actual = serde_json::to_value(&response)
                .unwrap_or_else(|failure| panic!("{} step {index} embedding result: {failure}", cassette.name));
            let Some(expected) = cassette.steps[index].result.as_ref() else {
                panic!("{} step {index} embedding result: the step recorded no result", cassette.name);
            };
            assert_eq!(actual, *expected, "{} step {index} embedding result", cassette.name);
        }
    }
}

pub async fn replay_decision_cassette(cassette: &Cassette) {
    for (index, step) in cassette.steps.iter().enumerate() {
        let Some(provider) = step.provider.as_ref() else {
            panic!("{} step {index} decision: the step recorded no provider", cassette.name);
        };
        assert_eq!(step.operation.as_deref(), Some("decision.generate"), "{} step {index} operation", cassette.name);
        let settings = Settings::new(provider.settings.clone());
        let runtime = ProviderPackages::builtin()
            .decision(&provider.package, &provider.model_id, &settings, &MapEnv::default())
            .unwrap_or_else(|error| {
                panic!("{} step {index} provider {}: {}", cassette.name, provider.package, error.message())
            });
        let RuntimeModel::Decision(model) = runtime else {
            panic!("{} step {index} decision: expected a decision model", cassette.name);
        };
        assert_eq!(model.id, step.model["id"], "{} step {index} decision model.id", cassette.name);
        assert_eq!(model.provider, step.model["provider"], "{} step {index} decision model.provider", cassette.name);
        assert_eq!(model.route.id, step.model["route"]["id"], "{} step {index} decision model.route.id", cassette.name);
        let input: DecisionRequestInput = serde_json::from_value(step.request.clone())
            .unwrap_or_else(|error| panic!("{} step {index} decision request: {error}", cassette.name));
        let request = input.with_model(model);
        let exchanges = match step.exchanges.as_ref() {
            Some(exchanges) if !exchanges.is_empty() => exchanges.clone(),
            _ => panic!("{} step {index} decision exchanges: the step recorded no HTTP exchange", cassette.name),
        };
        let sender = Arc::new(RecordedSender {
            cassette: cassette.name.clone(),
            step: index,
            label: "decision".to_string(),
            chunk: None,
            exchanges: Mutex::new(VecDeque::from(exchanges)),
        });
        let clock = Arc::new(FixedClock::new(cassette.clock_epoch_ms));
        let context = ExecutionContext::new(clock.clone(), Arc::new(SequenceIds::new()), Arc::new(MapEnv::default()));
        let executor = RequestExecutor::new(sender.clone(), None, clock);
        let outcome = DecisionClient::generate(request, &executor, &context).await;
        sender.assert_consumed();
        assert_decision_outcome(cassette, index, step.error.as_ref(), outcome);
    }
}

pub async fn replay_decision_group(group: &str) -> usize {
    let corpus = crate::corpus::Corpus::load();
    let mut steps = 0;
    for cassette in corpus.cassettes.iter().filter(|cassette| cassette.group == group) {
        replay_decision_cassette(cassette).await;
        steps += cassette.steps.len();
    }
    steps
}

fn assert_decision_outcome(
    cassette: &Cassette,
    index: usize,
    recorded_error: Option<&crate::corpus::StepError>,
    outcome: Result<opencode_ai::modalities::decision::DecisionResponse, AiError>,
) {
    match recorded_error {
        Some(recorded) => {
            let error = match outcome {
                Err(error) => error,
                Ok(_) => panic!("{} step {index} decision error: the generate call succeeded", cassette.name),
            };
            let reason = serde_json::to_value(&error.reason)
                .unwrap_or_else(|failure| panic!("{} step {index} decision error.reason: {failure}", cassette.name));
            assert_eq!(reason, recorded.reason, "{} step {index} decision error.reason", cassette.name);
            assert_eq!(
                error.is_retryable(),
                recorded.retryable,
                "{} step {index} decision error.retryable",
                cassette.name
            );
        }
        None => {
            let response = match outcome {
                Ok(response) => response,
                Err(error) => {
                    panic!("{} step {index} decision result: the generate call failed with {error}", cassette.name)
                }
            };
            let actual = serde_json::to_value(&response)
                .unwrap_or_else(|failure| panic!("{} step {index} decision result: {failure}", cassette.name));
            let Some(expected) = cassette.steps[index].result.as_ref() else {
                panic!("{} step {index} decision result: the step recorded no result", cassette.name);
            };
            assert_eq!(actual, *expected, "{} step {index} decision result", cassette.name);
        }
    }
}

pub async fn replay_image_cassette(cassette: &Cassette) {
    for (index, step) in cassette.steps.iter().enumerate() {
        let Some(provider) = step.provider.as_ref() else {
            panic!("{} step {index} image: the step recorded no provider", cassette.name);
        };
        let operation = step.operation.as_deref().unwrap_or_default();
        assert!(
            matches!(operation, "image.generate" | "image.stream"),
            "{} step {index} image: unexpected operation {operation}",
            cassette.name
        );
        let settings = Settings::new(provider.settings.clone());
        let runtime = ProviderPackages::builtin()
            .model(&provider.package, &provider.model_id, &settings, &MapEnv::default())
            .unwrap_or_else(|error| {
                panic!("{} step {index} provider {}: {}", cassette.name, provider.package, error.message())
            });
        let RuntimeModel::Image(model) = runtime else {
            panic!("{} step {index} image: expected an image model", cassette.name);
        };
        assert_eq!(model.id, step.model["id"], "{} step {index} image model.id", cassette.name);
        assert_eq!(model.provider, step.model["provider"], "{} step {index} image model.provider", cassette.name);
        assert_eq!(model.route.id(), step.model["route"]["id"], "{} step {index} image model.route.id", cassette.name);
        let input: ImageRequestInput = serde_json::from_value(step.request.clone())
            .unwrap_or_else(|error| panic!("{} step {index} image request: {error}", cassette.name));
        let exchanges = match step.exchanges.as_ref() {
            Some(exchanges) if !exchanges.is_empty() => exchanges.clone(),
            _ => panic!("{} step {index} image exchanges: the step recorded no HTTP exchange", cassette.name),
        };
        for chunk in [None, Some(CHUNKED_BODY_SIZE)] {
            let label = match chunk {
                None => "single-chunk".to_string(),
                Some(size) => format!("{size}-byte-chunk"),
            };
            let sender = Arc::new(RecordedSender {
                cassette: cassette.name.clone(),
                step: index,
                label: label.clone(),
                chunk,
                exchanges: Mutex::new(VecDeque::from(exchanges.clone())),
            });
            let clock = Arc::new(FixedClock::new(cassette.clock_epoch_ms));
            let context =
                ExecutionContext::new(clock.clone(), Arc::new(SequenceIds::new()), Arc::new(MapEnv::default()));
            let executor = RequestExecutor::new(sender.clone(), None, clock);
            let request = input.clone().with_model(model.clone());
            if operation == "image.generate" {
                let outcome = ImageClient::generate(request, &executor, &context).await;
                sender.assert_consumed();
                assert_image_generate_outcome(cassette, index, &label, step, outcome);
            } else {
                let items = ImageClient::stream(request, &executor, &context).collect::<Vec<_>>().await;
                sender.assert_consumed();
                assert_image_stream_outcome(cassette, index, &label, step, items);
            }
        }
    }
}

pub async fn replay_image_group(group: &str) -> usize {
    let corpus = crate::corpus::Corpus::load();
    let mut steps = 0;
    for cassette in corpus.cassettes.iter().filter(|cassette| cassette.group == group) {
        replay_image_cassette(cassette).await;
        steps += cassette.steps.len();
    }
    steps
}

fn assert_image_generate_outcome(
    cassette: &Cassette,
    index: usize,
    label: &str,
    step: &Step,
    outcome: Result<opencode_ai::modalities::image::ImageResponse, AiError>,
) {
    match &step.error {
        Some(recorded) => {
            let error = match outcome {
                Err(error) => error,
                Ok(_) => panic!("{} step {index} {label} image error: the generate call succeeded", cassette.name),
            };
            assert_recorded_error(cassette, index, label, recorded, &error);
        }
        None => {
            let response = match outcome {
                Ok(response) => response,
                Err(error) => {
                    panic!("{} step {index} {label} image result: the generate call failed with {error}", cassette.name)
                }
            };
            let actual = serde_json::to_value(&response)
                .unwrap_or_else(|failure| panic!("{} step {index} {label} image result: {failure}", cassette.name));
            let Some(expected) = step.result.as_ref() else {
                panic!("{} step {index} {label} image result: the step recorded no result", cassette.name);
            };
            assert_eq!(actual, *expected, "{} step {index} {label} image result", cassette.name);
        }
    }
}

fn assert_image_stream_outcome(
    cassette: &Cassette,
    index: usize,
    label: &str,
    step: &Step,
    items: Vec<Result<ImageEvent, AiError>>,
) {
    match &step.error {
        Some(recorded) => {
            let error = match items.as_slice() {
                [Err(error)] => error,
                _ => panic!("{} step {index} {label} image error: expected only the recorded error", cassette.name),
            };
            assert_recorded_error(cassette, index, label, recorded, error);
        }
        None => {
            let events = items
                .into_iter()
                .map(|item| match item {
                    Ok(event) => event,
                    Err(error) => {
                        panic!("{} step {index} {label} image events: the stream failed with {error}", cassette.name)
                    }
                })
                .map(|event| {
                    serde_json::to_value(&event).unwrap_or_else(|failure| {
                        panic!("{} step {index} {label} image events: {failure}", cassette.name)
                    })
                })
                .collect::<Vec<Value>>();
            let Some(expected) = step.result.as_ref().and_then(Value::as_array) else {
                panic!("{} step {index} {label} image events: the step recorded no event list", cassette.name);
            };
            assert_eq!(events, *expected, "{} step {index} {label} image events", cassette.name);
        }
    }
}

fn assert_recorded_error(
    cassette: &Cassette,
    index: usize,
    label: &str,
    recorded: &crate::corpus::StepError,
    error: &AiError,
) {
    let reason = serde_json::to_value(&error.reason)
        .unwrap_or_else(|failure| panic!("{} step {index} {label} error.reason: {failure}", cassette.name));
    assert_eq!(reason, recorded.reason, "{} step {index} {label} error.reason", cassette.name);
    assert_eq!(error.is_retryable(), recorded.retryable, "{} step {index} {label} error.retryable", cassette.name);
}

fn assert_outcome(
    cassette: &Cassette,
    index: usize,
    label: &str,
    recorded_error: Option<&crate::corpus::StepError>,
    recorded_events: &[Value],
    items: Vec<Result<LlmEvent, AiError>>,
) {
    match recorded_error {
        Some(recorded) => {
            let error = match items.as_slice() {
                [Err(error)] => error,
                [] => panic!("{} step {index} {label} error: the stream produced no items", cassette.name),
                items => panic!(
                    "{} step {index} {label} error: the stream produced {} items, expected only the recorded error",
                    cassette.name,
                    items.len()
                ),
            };
            let reason = serde_json::to_value(&error.reason)
                .unwrap_or_else(|failure| panic!("{} step {index} {label} error.reason: {failure}", cassette.name));
            assert_eq!(reason, recorded.reason, "{} step {index} {label} error.reason", cassette.name);
            assert_eq!(
                error.is_retryable(),
                recorded.retryable,
                "{} step {index} {label} error.retryable",
                cassette.name
            );
        }
        None => {
            let events = items
                .iter()
                .map(|item| match item {
                    Ok(event) => event,
                    Err(error) => {
                        panic!("{} step {index} {label} events: the stream failed with {error}", cassette.name)
                    }
                })
                .collect::<Vec<&LlmEvent>>();
            let events = events
                .into_iter()
                .map(|event| {
                    serde_json::to_value(event)
                        .unwrap_or_else(|failure| panic!("{} step {index} {label} events: {failure}", cassette.name))
                })
                .collect::<Vec<Value>>();
            assert_eq!(events, recorded_events, "{} step {index} {label} events", cassette.name);
        }
    }
}

struct RecordedSender {
    cassette: String,
    step: usize,
    label: String,
    chunk: Option<usize>,
    exchanges: Mutex<VecDeque<Exchange>>,
}

impl RecordedSender {
    fn assert_consumed(&self) {
        let remaining = self.exchanges.lock().expect("recorded exchanges").len();
        assert_eq!(
            remaining, 0,
            "{} step {} {} exchanges: {remaining} recorded exchanges were not consumed",
            self.cassette, self.step, self.label
        );
    }
}

impl HttpSender for RecordedSender {
    fn send(&self, request: PreparedHttp) -> BoxFuture<Result<HttpResponse, AiError>> {
        let exchange = self.exchanges.lock().expect("recorded exchanges").pop_front();
        let Some(exchange) = exchange else {
            panic!(
                "{} step {} {} exchanges: the request had no recorded exchange left",
                self.cassette, self.step, self.label
            );
        };
        assert_prepared(&self.cassette, self.step, &self.label, &exchange.prepared, &request);
        let status = exchange.response.status;
        let headers = exchange.response.headers.into_iter().collect::<Vec<(String, String)>>();
        let body = body_bytes(
            &exchange.response.body,
            exchange.response.body_encoding.as_deref(),
            &self.cassette,
            self.step,
            &self.label,
            "response.body",
        );
        let chunks = chunk_bytes(body, self.chunk);
        let body: ByteStream = Box::pin(stream::iter(chunks.into_iter().map(Ok)));
        Box::pin(async move { Ok(HttpResponse::new(status, headers, body)) })
    }
}

fn assert_prepared(cassette: &str, step: usize, label: &str, prepared: &Prepared, request: &PreparedHttp) {
    if let Some(boundary) = prepared.boundary.as_deref() {
        assert_eq!(request.method, prepared.method, "{cassette} step {step} {label} prepared.method");
        assert_eq!(request.url, prepared.url, "{cassette} step {step} {label} prepared.url");
        let sent_boundary = request
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
            .and_then(|(_, value)| boundary_token(value))
            .unwrap_or_else(|| boundary.to_string());
        let recorded_boundary = prepared
            .headers
            .get("content-type")
            .and_then(|value| boundary_token(value))
            .unwrap_or_else(|| boundary.to_string());
        let sent = request
            .headers
            .iter()
            .map(|(name, value)| (name.to_lowercase(), normalize_boundary_header(name, value)))
            .collect::<BTreeMap<String, String>>();
        let recorded = prepared
            .headers
            .iter()
            .map(|(name, value)| (name.to_lowercase(), normalize_boundary_header(name, value)))
            .collect::<BTreeMap<String, String>>();
        assert_eq!(sent, recorded, "{cassette} step {step} {label} prepared.headers");
        let sent_body = replace_boundary(&request.body, &sent_boundary);
        let recorded_body = replace_boundary(
            &body_bytes(&prepared.body, prepared.body_encoding.as_deref(), cassette, step, label, "prepared.body"),
            &recorded_boundary,
        );
        assert_eq!(sent_body, recorded_body, "{cassette} step {step} {label} prepared.body bytes");
        return;
    }
    assert_eq!(request.method, prepared.method, "{cassette} step {step} {label} prepared.method");
    assert_eq!(request.url, prepared.url, "{cassette} step {step} {label} prepared.url");
    let headers = request
        .headers
        .iter()
        .map(|(name, value)| (name.to_lowercase(), value.clone()))
        .collect::<BTreeMap<String, String>>();
    let sent = request.body.to_vec();
    let recorded =
        body_bytes(&prepared.body, prepared.body_encoding.as_deref(), cassette, step, label, "prepared.body");
    assert_eq!(headers, prepared.headers, "{cassette} step {step} {label} prepared.headers");
    assert_eq!(sent, recorded, "{cassette} step {step} {label} prepared.body bytes");
    let sent_json: Value = serde_json::from_slice(&sent)
        .unwrap_or_else(|error| panic!("{cassette} step {step} {label} prepared.body: {error}"));
    let recorded_json: Value = serde_json::from_slice(&recorded)
        .unwrap_or_else(|error| panic!("{cassette} step {step} {label} prepared.body: {error}"));
    assert_eq!(sent_json, recorded_json, "{cassette} step {step} {label} prepared.body JSON");
}

fn body_bytes(body: &str, encoding: Option<&str>, cassette: &str, step: usize, label: &str, field: &str) -> Vec<u8> {
    match encoding {
        Some("base64") => BASE64_STANDARD
            .decode(body)
            .unwrap_or_else(|error| panic!("{cassette} step {step} {label} {field}: {error}")),
        Some(other) => panic!("{cassette} step {step} {label} {field}: unsupported encoding {other}"),
        None => body.as_bytes().to_vec(),
    }
}

fn chunk_bytes(body: Vec<u8>, chunk: Option<usize>) -> Vec<Bytes> {
    match chunk {
        None => vec![Bytes::from(body)],
        Some(size) => body.chunks(size).map(Bytes::copy_from_slice).collect(),
    }
}

fn normalize_boundary_header(name: &str, value: &str) -> String {
    if !name.eq_ignore_ascii_case("content-type") {
        return value.to_string();
    }
    let Some((prefix, rest)) = value.split_once("boundary=") else {
        return value.to_string();
    };
    let token = rest.split(';').next().unwrap_or_default();
    let suffix = &rest[token.len()..];
    format!("{prefix}boundary=BOUNDARY{suffix}")
}

fn boundary_token(value: &str) -> Option<String> {
    let (_, rest) = value.split_once("boundary=")?;
    let token = rest.split(';').next().unwrap_or_default().trim().trim_matches('"');
    Some(token.to_string())
}

fn replace_boundary(body: &[u8], token: &str) -> Vec<u8> {
    let needle = token.as_bytes();
    if needle.is_empty() {
        return body.to_vec();
    }
    let mut replaced = Vec::with_capacity(body.len());
    let mut rest = body;
    while let Some(position) = rest.windows(needle.len()).position(|window| window == needle) {
        replaced.extend_from_slice(&rest[..position]);
        replaced.extend_from_slice(b"BOUNDARY");
        rest = &rest[position + needle.len()..];
    }
    replaced.extend_from_slice(rest);
    replaced
}

struct ReplayChannelExecutor {
    cassette: String,
    steps: Vec<WebSocketRecord>,
    state: Arc<Mutex<ReplayState>>,
}

#[derive(Default)]
struct ReplayState {
    current: usize,
    connected: bool,
    executed: bool,
    checkpoint: Option<ChannelCheckpoint>,
    pending: Option<ChannelCheckpoint>,
}

impl ReplayChannelExecutor {
    fn new(cassette: &Cassette) -> ReplayChannelExecutor {
        ReplayChannelExecutor {
            cassette: cassette.name.clone(),
            steps: cassette.steps.iter().map(|step| step.websocket_record()).collect(),
            state: Arc::new(Mutex::new(ReplayState::default())),
        }
    }

    fn begin(&self, index: usize) {
        let mut state = self.state.lock().expect("replay channel state");
        for op in &self.steps[index].ops {
            match op {
                ChannelOp::Reconnect { preserve_checkpoint } => {
                    state.connected = false;
                    state.pending = None;
                    if !preserve_checkpoint {
                        state.checkpoint = None;
                    }
                }
            }
        }
        state.current = index;
        state.executed = false;
    }

    fn end(&self, index: usize) {
        let state = self.state.lock().expect("replay channel state");
        assert_eq!(state.current, index, "{} step {index} channel: ended on step {}", self.cassette, state.current);
        assert!(state.executed, "{} step {index} channel: the executor never ran", self.cassette);
    }
}

impl ChannelExecutor for ReplayChannelExecutor {
    fn execute(&self, exchange: ChannelExchange) -> BoxFuture<Result<ChannelExecution, AiError>> {
        let mut state = self.state.lock().expect("replay channel state");
        let index = state.current;
        let record = self.steps[index].clone();
        let ChannelExchange { connect, mut driver, .. } = exchange;
        if state.connected {
            assert!(
                record.connect.is_none(),
                "{} step {index} channel connect: the corpus recorded a connect while the connection was open",
                self.cassette
            );
        } else {
            let Some(connect_record) = record.connect else {
                panic!(
                    "{} step {index} channel connect: the corpus recorded no connect while the connection was closed",
                    self.cassette
                );
            };
            assert_eq!(connect.url, connect_record.url, "{} step {index} channel connect.url", self.cassette);
            let headers = connect
                .headers
                .into_iter()
                .map(|(name, value)| (name.to_lowercase(), value))
                .collect::<BTreeMap<String, String>>();
            let recorded = connect_record
                .headers
                .into_iter()
                .map(|(name, value)| (name.to_lowercase(), value))
                .collect::<BTreeMap<String, String>>();
            assert_eq!(headers, recorded, "{} step {index} channel connect.headers", self.cassette);
            state.connected = true;
        }
        let create = driver
            .create(state.checkpoint.as_ref())
            .unwrap_or_else(|error| panic!("{} step {index} channel create: {}", self.cassette, error.message()));
        if matches!(create.mode, ChannelMode::Full) {
            state.checkpoint = None;
        }
        state.pending = None;
        state.executed = true;
        assert_eq!(record.sent, vec![create.message.clone()], "{} step {index} channel sent", self.cassette);
        let mut items: Vec<Result<String, AiError>> = Vec::new();
        for entry in &record.received {
            let kind = entry.get("type").and_then(Value::as_str).unwrap_or_default();
            match kind {
                "frame" | "completed" | "incomplete" => {
                    let frame = entry.get("frame").and_then(Value::as_str).unwrap_or_else(|| {
                        panic!("{} step {index} channel {kind}: the entry recorded no frame", self.cassette)
                    });
                    let observation = driver.observe(&create, frame).unwrap_or_else(|error| {
                        panic!("{} step {index} channel observe: {}", self.cassette, error.message())
                    });
                    match (kind, observation) {
                        ("frame", ChannelObservation::Frame { frame: observed }) => {
                            assert_eq!(observed, frame, "{} step {index} channel frame", self.cassette);
                        }
                        ("completed", ChannelObservation::Completed { frame: observed, checkpoint }) => {
                            assert_eq!(observed, frame, "{} step {index} channel completed.frame", self.cassette);
                            if checkpoint.is_some() {
                                state.pending = checkpoint;
                            }
                        }
                        ("incomplete", ChannelObservation::Incomplete { frame: observed }) => {
                            assert_eq!(observed, frame, "{} step {index} channel incomplete.frame", self.cassette);
                            state.checkpoint = None;
                        }
                        _ => panic!(
                            "{} step {index} channel {kind}: the driver observed a different variant",
                            self.cassette
                        ),
                    }
                    items.push(Ok(frame.to_string()));
                }
                "rejected" | "provider-failure" => {
                    let frame = entry["reason"]["body"].as_str().unwrap_or_else(|| {
                        panic!("{} step {index} channel {kind}: the entry recorded no reason.body", self.cassette)
                    });
                    let observation = driver.observe(&create, frame).unwrap_or_else(|error| {
                        panic!("{} step {index} channel observe: {}", self.cassette, error.message())
                    });
                    let retryable = entry.get("retryable").and_then(Value::as_bool).unwrap_or_else(|| {
                        panic!("{} step {index} channel {kind}: the entry recorded no retryable", self.cassette)
                    });
                    match (kind, observation) {
                        ("rejected", ChannelObservation::Rejected { error, recovery: observed }) => {
                            let recorded = entry.get("recovery").cloned().unwrap_or_else(|| {
                                panic!(
                                    "{} step {index} channel rejected: the entry recorded no recovery",
                                    self.cassette
                                )
                            });
                            let recorded = serde_json::from_value::<Recovery>(recorded).unwrap_or_else(|error| {
                                panic!("{} step {index} channel rejected.recovery: {error}", self.cassette)
                            });
                            assert_eq!(observed, recorded, "{} step {index} channel rejected.recovery", self.cassette);
                            assert_eq!(
                                serde_json::to_value(&error.reason).unwrap_or_else(|failure| panic!(
                                    "{} step {index} channel rejected.reason: {failure}",
                                    self.cassette
                                )),
                                entry["reason"],
                                "{} step {index} channel rejected.reason",
                                self.cassette
                            );
                            assert_eq!(
                                error.is_retryable(),
                                retryable,
                                "{} step {index} channel rejected.retryable",
                                self.cassette
                            );
                            state.checkpoint = None;
                            items.push(Err(error));
                        }
                        ("provider-failure", ChannelObservation::ProviderFailure { error }) => {
                            assert_eq!(
                                serde_json::to_value(&error.reason).unwrap_or_else(|failure| panic!(
                                    "{} step {index} channel provider-failure.reason: {failure}",
                                    self.cassette
                                )),
                                entry["reason"],
                                "{} step {index} channel provider-failure.reason",
                                self.cassette
                            );
                            assert_eq!(
                                error.is_retryable(),
                                retryable,
                                "{} step {index} channel provider-failure.retryable",
                                self.cassette
                            );
                            state.checkpoint = None;
                            items.push(Err(error));
                        }
                        _ => panic!(
                            "{} step {index} channel {kind}: the driver observed a different variant",
                            self.cassette
                        ),
                    }
                }
                other => panic!("{} step {index} channel: unknown received type {other}", self.cassette),
            }
            if matches!(kind, "completed" | "incomplete" | "rejected" | "provider-failure") {
                break;
            }
        }
        let mut pending = state.pending.take();
        let shared = self.state.clone();
        drop(state);
        let complete: BoxFuture<()> = Box::pin(async move {
            let mut state = shared.lock().expect("replay channel state");
            state.checkpoint = pending.take();
            state.pending = None;
        });
        Box::pin(async move { Ok(ChannelExecution { frames: Box::pin(stream::iter(items)), http: None, complete }) })
    }
}

struct UnusedSender;

impl HttpSender for UnusedSender {
    fn send(&self, request: PreparedHttp) -> BoxFuture<Result<HttpResponse, AiError>> {
        panic!("{}: the websocket channel sent an HTTP request", request.url);
    }
}

async fn replay_websocket_cassette(cassette: &Cassette) {
    let registry = ProtocolRegistry::builtin();
    let channel = Arc::new(ReplayChannelExecutor::new(cassette));
    for (index, step) in cassette.steps.iter().enumerate() {
        let model = model_for_step(cassette, index, step, &registry);
        let recorded_events = match step.events.as_ref() {
            Some(events) => events.clone(),
            None if step.error.is_some() => Vec::new(),
            None => panic!("{} step {index} events: the step recorded no events and no error", cassette.name),
        };
        channel.begin(index);
        let clock = Arc::new(FixedClock::new(cassette.clock_epoch_ms));
        let context = ExecutionContext::new(clock.clone(), Arc::new(SequenceIds::new()), Arc::new(MapEnv::default()));
        let client = LlmClient::new(RequestExecutor::new(Arc::new(UnusedSender), None, clock), context);
        let options = StreamOptions { web_socket: Some(channel.clone()), ..Default::default() };
        let items = client.stream(&model, step.request(), options).collect::<Vec<Result<LlmEvent, AiError>>>().await;
        channel.end(index);
        assert_outcome(cassette, index, "websocket", step.error.as_ref(), &recorded_events, items);
    }
}

#[tokio::test]
async fn every_in_scope_cassette_replays() {
    let corpus = crate::corpus::Corpus::load();
    assert_eq!(corpus.cassettes.len(), 286, "in-scope cassette count");
    let groups = [
        "anthropic-messages",
        "bedrock-converse",
        "chat-variants",
        "decisions",
        "embeddings",
        "gateways",
        "gemini",
        "images",
        "native-vendors",
        "open-responses",
        "openai-chat",
        "openai-responses-websocket",
        "vendor-variants",
        "wrapped-core",
    ];
    for cassette in &corpus.cassettes {
        assert!(groups.contains(&cassette.group.as_str()), "{} has unexpected group {}", cassette.name, cassette.group);
    }
    for group in groups {
        assert!(corpus.cassettes.iter().any(|cassette| cassette.group == group), "{group} has no cassettes");
    }
    let mut replayed = 0;
    for cassette in &corpus.cassettes {
        if cassette.group == "embeddings" {
            replay_embedding_cassette(cassette).await;
        } else if cassette.group == "decisions" {
            replay_decision_cassette(cassette).await;
        } else if cassette.group == "images" {
            replay_image_cassette(cassette).await;
        } else if cassette.group == "openai-responses-websocket" {
            replay_websocket_cassette(cassette).await;
        } else {
            replay_cassette(cassette).await;
        }
        replayed += 1;
    }
    assert_eq!(replayed, 286, "replayed cassette count");
}

#[tokio::test]
async fn image_cassettes() {
    let corpus = crate::corpus::Corpus::load();
    let cassettes = corpus.cassettes.iter().filter(|cassette| cassette.group == "images").count();
    assert_eq!(cassettes, 22, "image cassette count");
    let steps = replay_image_group("images").await;
    assert_eq!(steps, 22, "image step count");
}

#[tokio::test]
async fn decision_cassettes() {
    let corpus = crate::corpus::Corpus::load();
    let cassettes = corpus.cassettes.iter().filter(|cassette| cassette.group == "decisions").count();
    assert_eq!(cassettes, 9, "decision cassette count");
    let steps = replay_decision_group("decisions").await;
    assert_eq!(steps, 9, "decision step count");
}

#[tokio::test]
async fn embedding_cassettes() {
    let corpus = crate::corpus::Corpus::load();
    let cassettes = corpus.cassettes.iter().filter(|cassette| cassette.group == "embeddings").count();
    assert_eq!(cassettes, 7, "embedding cassette count");
    let steps = replay_embedding_group("embeddings").await;
    assert_eq!(steps, 8, "embedding step count");
}

#[tokio::test]
async fn chat_variant_cassettes() {
    let corpus = crate::corpus::Corpus::load();
    let cassettes = corpus.cassettes.iter().filter(|cassette| cassette.group == "chat-variants").count();
    assert_eq!(cassettes, 91, "chat-variant cassette count");
    let steps = replay_group("chat-variants").await;
    assert_eq!(steps, 129, "chat-variant step count");
}

#[tokio::test]
async fn vendor_variant_cassettes() {
    let corpus = crate::corpus::Corpus::load();
    let cassettes = corpus.cassettes.iter().filter(|cassette| cassette.group == "vendor-variants").count();
    assert_eq!(cassettes, 66, "vendor-variant cassette count");
    let steps = replay_group("vendor-variants").await;
    assert_eq!(steps, 79, "vendor-variant step count");
}

#[tokio::test]
async fn native_vendor_cassettes() {
    let corpus = crate::corpus::Corpus::load();
    let cassettes = corpus.cassettes.iter().filter(|cassette| cassette.group == "native-vendors").count();
    assert_eq!(cassettes, 11, "native-vendor cassette count");
    let steps = replay_group("native-vendors").await;
    assert_eq!(steps, 14, "native-vendor step count");
}

#[tokio::test]
async fn gateway_cassettes() {
    let corpus = crate::corpus::Corpus::load();
    let cassettes = corpus.cassettes.iter().filter(|cassette| cassette.group == "gateways").count();
    assert_eq!(cassettes, 17, "gateway cassette count");
    let steps = replay_group("gateways").await;
    assert_eq!(steps, 27, "gateway step count");
}

#[tokio::test]
async fn wrapped_core_cassettes() {
    let corpus = crate::corpus::Corpus::load();
    let cassettes = corpus.cassettes.iter().filter(|cassette| cassette.group == "wrapped-core").count();
    assert_eq!(cassettes, 12, "wrapped-core cassette count");
    let steps = replay_group("wrapped-core").await;
    assert_eq!(steps, 14, "wrapped-core step count");
}

#[tokio::test]
async fn responses_websocket_cassettes() {
    let corpus = crate::corpus::Corpus::load();
    let cassettes = corpus
        .cassettes
        .iter()
        .filter(|cassette| cassette.group == "openai-responses-websocket")
        .collect::<Vec<&Cassette>>();
    assert_eq!(cassettes.len(), 3, "websocket cassette count");
    let mut steps = 0;
    for cassette in cassettes {
        replay_websocket_cassette(cassette).await;
        steps += cassette.steps.len();
    }
    assert_eq!(steps, 7, "websocket step count");
}

#[tokio::test]
async fn openai_chat_cassettes() {
    let corpus = crate::corpus::Corpus::load();
    let cassettes = corpus.cassettes.iter().filter(|cassette| cassette.group == "openai-chat").count();
    assert_eq!(cassettes, 11, "openai-chat cassette count");
    let steps = replay_group("openai-chat").await;
    assert_eq!(steps, 15, "openai-chat step count");
}

#[tokio::test]
async fn anthropic_messages_cassettes() {
    let corpus = crate::corpus::Corpus::load();
    let cassettes = corpus.cassettes.iter().filter(|cassette| cassette.group == "anthropic-messages").count();
    assert_eq!(cassettes, 8, "anthropic-messages cassette count");
    let steps = replay_group("anthropic-messages").await;
    assert_eq!(steps, 11, "anthropic-messages step count");
}

#[tokio::test]
async fn bedrock_converse_cassettes() {
    let corpus = crate::corpus::Corpus::load();
    let cassettes = corpus.cassettes.iter().filter(|cassette| cassette.group == "bedrock-converse").count();
    assert_eq!(cassettes, 13, "bedrock-converse cassette count");
    let steps = replay_group("bedrock-converse").await;
    assert_eq!(steps, 17, "bedrock-converse step count");
}

#[tokio::test]
async fn gemini_cassettes() {
    let corpus = crate::corpus::Corpus::load();
    let cassettes = corpus.cassettes.iter().filter(|cassette| cassette.group == "gemini").count();
    assert_eq!(cassettes, 4, "gemini cassette count");
    let steps = replay_group("gemini").await;
    assert_eq!(steps, 6, "gemini step count");
}

#[tokio::test]
async fn open_responses_cassettes() {
    let corpus = crate::corpus::Corpus::load();
    let cassettes = corpus
        .cassettes
        .iter()
        .filter(|cassette| cassette.group == "open-responses" && cassette.protocol == "open-responses")
        .collect::<Vec<&Cassette>>();
    assert_eq!(cassettes.len(), 3, "open-responses cassette count");
    let mut steps = 0;
    for cassette in cassettes {
        replay_cassette(cassette).await;
        steps += cassette.steps.len();
    }
    assert_eq!(steps, 4, "open-responses step count");
}

#[tokio::test]
async fn responses_http_cassettes() {
    let corpus = crate::corpus::Corpus::load();
    let cassettes = corpus.cassettes.iter().filter(|cassette| cassette.group == "open-responses").count();
    assert_eq!(cassettes, 12, "responses-http cassette count");
    let steps = replay_group("open-responses").await;
    assert_eq!(steps, 18, "responses-http step count");
}

#[tokio::test]
async fn openai_responses_cassettes() {
    let corpus = crate::corpus::Corpus::load();
    let cassettes = corpus
        .cassettes
        .iter()
        .filter(|cassette| cassette.group == "open-responses" && cassette.protocol == "openai-responses")
        .collect::<Vec<&Cassette>>();
    assert_eq!(cassettes.len(), 9, "openai-responses cassette count");
    let mut steps = 0;
    for cassette in cassettes {
        replay_cassette(cassette).await;
        steps += cassette.steps.len();
    }
    assert_eq!(steps, 14, "openai-responses step count");
}
