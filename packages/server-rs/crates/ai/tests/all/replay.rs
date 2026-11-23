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
use opencode_ai::route::client::LlmClient;
use opencode_ai::route::compile::StreamOptions;
use opencode_ai::route::executor::{BoxFuture, ByteStream, HttpResponse, HttpSender, PreparedHttp, RequestExecutor};
use opencode_ai::route::model::{Model, ModelDescriptor, ProtocolRegistry};
use opencode_ai::route::transport::channel::{
    ChannelCheckpoint, ChannelExchange, ChannelExecution, ChannelExecutor, ChannelMode, ChannelObservation,
};
use opencode_ai::schema::{AiError, LlmEvent, Recovery};

use crate::corpus::{Cassette, ChannelOp, Exchange, Prepared, WebSocketRecord};

const CHUNKED_BODY_SIZE: usize = 7;

pub async fn replay_cassette(cassette: &Cassette) {
    let registry = ProtocolRegistry::builtin();
    for (index, step) in cassette.steps.iter().enumerate() {
        let descriptor: ModelDescriptor = serde_json::from_value(step.model.clone())
            .unwrap_or_else(|error| panic!("{} step {index} model descriptor: {error}", cassette.name));
        let model = Model::from_descriptor(descriptor, &registry)
            .unwrap_or_else(|error| panic!("{} step {index} model: {}", cassette.name, error.message()));
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

pub async fn replay_group(group: &str) -> usize {
    let corpus = crate::corpus::Corpus::load();
    let mut steps = 0;
    for cassette in corpus.cassettes.iter().filter(|cassette| cassette.group == group) {
        replay_cassette(cassette).await;
        steps += cassette.steps.len();
    }
    steps
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
            let error = match items.last() {
                Some(Err(error)) => error,
                Some(Ok(event)) => panic!(
                    "{} step {index} {label} error: the last item is {}, not the recorded error",
                    cassette.name,
                    event_type(event)
                ),
                None => panic!("{} step {index} {label} error: the stream produced no items", cassette.name),
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

fn event_type(event: &LlmEvent) -> String {
    serde_json::to_value(event).ok().and_then(|value| value.get("type").cloned()).unwrap_or_default().to_string()
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
    assert_eq!(request.method, prepared.method, "{cassette} step {step} {label} prepared.method");
    assert_eq!(request.url, prepared.url, "{cassette} step {step} {label} prepared.url");
    let headers = request
        .headers
        .iter()
        .map(|(name, value)| (name.to_lowercase(), value.clone()))
        .collect::<BTreeMap<String, String>>();
    assert_eq!(headers, prepared.headers, "{cassette} step {step} {label} prepared.headers");
    let sent = request.body.to_vec();
    let recorded =
        body_bytes(&prepared.body, prepared.body_encoding.as_deref(), cassette, step, label, "prepared.body");
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
        let descriptor: ModelDescriptor = serde_json::from_value(step.model.clone())
            .unwrap_or_else(|error| panic!("{} step {index} model descriptor: {error}", cassette.name));
        let model = Model::from_descriptor(descriptor, &registry)
            .unwrap_or_else(|error| panic!("{} step {index} model: {}", cassette.name, error.message()));
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
