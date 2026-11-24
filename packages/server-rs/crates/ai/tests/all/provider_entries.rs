#![allow(clippy::result_large_err)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use base64::prelude::{BASE64_STANDARD, Engine};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use opencode_ai::ExecutionContext;
use opencode_ai::MapEnv;
use opencode_ai::clock::FixedClock;
use opencode_ai::id::SequenceIds;
use opencode_ai::modalities::decision::{DecisionClient, DecisionPart, DecisionQuestion, DecisionRequestInput};
use opencode_ai::modalities::embedding::{EmbeddingClient, EmbeddingRequestInput, EmbeddingValue};
use opencode_ai::modalities::image::{ImageClient, ImageRequestInput};
use opencode_ai::modalities::speech::{SpeechClient, SpeechRequestInput, SpeechVoice};
use opencode_ai::modalities::transcription::{TranscriptionClient, TranscriptionRequestInput};
use opencode_ai::modalities::video::{VideoClient, VideoRequestInput};
use opencode_ai::providers::{ProviderPackages, RuntimeModel, Settings};
use opencode_ai::route::compile::{StreamOptions, compile};
use opencode_ai::route::executor::{BoxFuture, HttpResponse, HttpSender, PreparedHttp, RequestExecutor};
use opencode_ai::schema::{
    AiError, AiErrorReason, LlmRequest, MediaAsset, MediaSource, TransportKind, TransportOperation,
};

const FIXTURE_BOUNDARY: &str = "----OpenCodeFixtureBoundary";
const FIXTURE_AUDIO: &str = "UklGRiQAAABXQVZFZm10IBAAAAABAAEAQB8AAEAfAAABAAgAZGF0YQAAAAA=";
const SPEECH_CARTESIA_VOICE: &str = "f786b574-daa5-4673-aa0c-cbe3e8534c02";
const SPEECH_ELEVENLABS_VOICE: &str = "JBFqnCBsd6RMkjVDRZzb";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Document {
    clock_epoch_ms: u64,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    name: String,
    specifier: String,
    #[serde(rename = "modelID")]
    model_id: String,
    settings: Map<String, Value>,
    env: BTreeMap<String, String>,
    kind: String,
    route_id: String,
    descriptor: Option<Value>,
    prepared: Option<Prepared>,
    error: Option<CaseError>,
}

#[derive(Deserialize)]
struct Prepared {
    method: String,
    url: String,
    headers: BTreeMap<String, String>,
    body: String,
}

#[derive(Deserialize)]
struct CaseError {
    message: String,
}

struct CaptureSender {
    request: Mutex<Option<PreparedHttp>>,
}

impl HttpSender for CaptureSender {
    fn send(&self, request: PreparedHttp) -> BoxFuture<Result<HttpResponse, AiError>> {
        *self.request.lock().unwrap() = Some(request);
        Box::pin(async {
            Err(AiError {
                reason: AiErrorReason::Transport {
                    message: "fixture capture".to_string(),
                    body: None,
                    http: None,
                    transport: TransportKind::Http,
                    operation: TransportOperation::Request,
                    code: None,
                    url: None,
                    phase: None,
                    delivery: None,
                    recovery: None,
                },
            })
        })
    }
}

#[tokio::test]
async fn provider_entries_match_typescript() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/provider-entries.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let document: Document = serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let packages = ProviderPackages::builtin();
    let mut failures = Vec::new();

    for case in &document.cases {
        let settings = Settings::new(case.settings.clone());
        let env = MapEnv::new(case.env.clone());
        let result = match case.kind.as_str() {
            "decision" => packages.decision(&case.specifier, &case.model_id, &settings, &env),
            "speech" => packages.speech(&case.specifier, &case.model_id, &settings, &env),
            "transcription" => packages.transcription(&case.specifier, &case.model_id, &settings, &env),
            _ => packages.model(&case.specifier, &case.model_id, &settings, &env),
        };
        if let Some(expected) = &case.error {
            match result {
                Ok(_) => failures.push(format!("{}: expected error {}", case.name, expected.message)),
                Err(error) if error.message() != expected.message => {
                    failures.push(format!("{}: error {:?} != {:?}", case.name, error.message(), expected.message))
                }
                Err(_) => {}
            }
            continue;
        }
        let runtime = match result {
            Ok(runtime) => runtime,
            Err(error) => {
                failures.push(format!("{}: {}", case.name, error.message()));
                continue;
            }
        };
        if kind_of(&runtime) != case.kind {
            failures.push(format!("{}: kind {}", case.name, kind_of(&runtime)));
        }
        if identity(&runtime).1 != case.route_id {
            failures.push(format!("{}: route id {}", case.name, identity(&runtime).1));
        }
        let actual_descriptor = descriptor(&runtime);
        if let Some(expected_descriptor) = case.descriptor.clone()
            && actual_descriptor != expected_descriptor
        {
            failures.push(format!(
                "{}: descriptor {} != {}",
                case.name,
                serde_json::to_string(&actual_descriptor).unwrap_or_default(),
                serde_json::to_string(&expected_descriptor).unwrap_or_default()
            ));
        }
        let Some(prepared) = case.prepared.as_ref() else {
            failures.push(format!("{}: no prepared request", case.name));
            continue;
        };
        let clock = Arc::new(FixedClock::new(document.clock_epoch_ms));
        let environment = Arc::new(MapEnv::new(case.env.clone()));
        let context = ExecutionContext::new(clock.clone(), Arc::new(SequenceIds::new()), environment);
        let request = prepare(&runtime, clock, &context).await;
        if let Some(message) = compare_prepared(prepared, &request) {
            failures.push(format!("{}: {message}", case.name));
        }
    }
    assert!(failures.is_empty(), "{} provider entry mismatches:\n{}", failures.len(), failures.join("\n"));
}

fn kind_of(runtime: &RuntimeModel) -> &'static str {
    match runtime {
        RuntimeModel::Language(_) => "language",
        RuntimeModel::Embedding(_) => "embedding",
        RuntimeModel::Decision(_) => "decision",
        RuntimeModel::Image(_) => "image",
        RuntimeModel::Video(_) => "video",
        RuntimeModel::Speech(_) => "speech",
        RuntimeModel::Transcription(_) => "transcription",
        RuntimeModel::Extraction(_) => "extraction",
    }
}

fn identity(runtime: &RuntimeModel) -> (String, String) {
    match runtime {
        RuntimeModel::Language(model) => (model.provider.clone(), model.route.id.clone()),
        RuntimeModel::Embedding(model) => (model.provider.clone(), model.route.id.clone()),
        RuntimeModel::Decision(model) => (model.provider.clone(), model.route.id.clone()),
        RuntimeModel::Image(model) => (model.provider.clone(), model.route.id().to_string()),
        RuntimeModel::Video(model) => (model.provider.clone(), model.route.id.clone()),
        RuntimeModel::Speech(model) => (model.provider.clone(), model.route.id().to_string()),
        RuntimeModel::Transcription(model) => (model.provider.clone(), model.route.id().to_string()),
        RuntimeModel::Extraction(model) => (model.provider.clone(), model.route.id.to_string()),
    }
}

fn descriptor(runtime: &RuntimeModel) -> Value {
    match runtime {
        RuntimeModel::Language(model) => {
            let mut value =
                serde_json::to_value(model.descriptor()).unwrap_or_else(|error| panic!("descriptor: {error}"));
            if let Some(object) = value.as_object_mut() {
                object.remove("options");
                object.remove("modelOptions");
                object.remove("transport");
                object.remove("auth");
            }
            value
        }
        other => {
            let (provider, route_id) = identity(other);
            json!({ "provider": provider, "routeId": route_id })
        }
    }
}

async fn prepare(runtime: &RuntimeModel, clock: Arc<FixedClock>, context: &ExecutionContext) -> PreparedHttp {
    let sender = Arc::new(CaptureSender { request: Mutex::new(None) });
    let executor = RequestExecutor::new(sender.clone(), None, clock);
    match runtime {
        RuntimeModel::Language(model) => {
            let compiled = compile(model, request("Hi"), &StreamOptions::default(), context)
                .unwrap_or_else(|error| panic!("compile: {}", error.message()));
            return compiled.prepared.request;
        }
        RuntimeModel::Embedding(model) => {
            let input = EmbeddingRequestInput {
                values: vec![EmbeddingValue::Text("fixture".to_string())],
                dimensions: None,
                input_type: None,
                provider_options: None,
                headers: None,
                timeout: None,
                http: None,
            };
            let _ = EmbeddingClient::generate(input.with_model(model.clone()), &executor, context).await;
        }
        RuntimeModel::Decision(model) => {
            let input = DecisionRequestInput {
                input: vec![DecisionPart::Text { text: "fixture".to_string() }],
                questions: vec![DecisionQuestion::Predicate {
                    name: "ok".to_string(),
                    instructions: "Say whether the text is the word fixture".to_string(),
                    criteria: None,
                }],
                provider_options: None,
                headers: None,
                timeout: None,
                http: None,
            };
            let _ = DecisionClient::generate(input.with_model(model.clone()), &executor, context).await;
        }
        RuntimeModel::Image(model) => {
            let input = ImageRequestInput {
                prompt: "fixture".to_string(),
                images: Vec::new(),
                n: None,
                aspect: None,
                resolution: None,
                provider_options: None,
                headers: None,
                timeout: None,
                http: None,
            };
            let _ = ImageClient::generate(input.with_model(model.clone()), &executor, context).await;
        }
        RuntimeModel::Video(model) => {
            let input = VideoRequestInput {
                prompt: "fixture".to_string(),
                start: None,
                end: None,
                references: Vec::new(),
                duration: None,
                aspect: None,
                resolution: None,
                sound: None,
                provider_options: None,
                headers: None,
                timeout: None,
                http: None,
            };
            let _ = VideoClient::start(input.with_model(model.clone()), &executor, context).await;
        }
        RuntimeModel::Speech(model) => {
            let input = SpeechRequestInput {
                text: "fixture".to_string(),
                voice: speech_voice(&model.provider).map(|id| SpeechVoice::Id { id }),
                format: None,
                speed: None,
                language: None,
                instructions: None,
                timestamps: None,
                provider_options: None,
                headers: None,
                timeout: None,
                http: None,
            };
            let _ = SpeechClient::generate(input.with_model(model.clone()), &executor, context).await;
        }
        RuntimeModel::Transcription(model) => {
            let input = TranscriptionRequestInput {
                audio: MediaAsset {
                    source: MediaSource::Base64 {
                        data: FIXTURE_AUDIO.to_string(),
                        media_type: "audio/wav".to_string(),
                    },
                    info: None,
                    provider_metadata: None,
                    headers: None,
                },
                language: None,
                prompt: None,
                timestamps: None,
                diarize: None,
                speakers: None,
                provider_options: None,
                headers: None,
                timeout: None,
                http: None,
            };
            let _ = TranscriptionClient::generate(input.with_model(model.clone()), None, &executor, context).await;
        }
        RuntimeModel::Extraction(_) => panic!("extraction has no provider entry specifier"),
    }
    sender.request.lock().unwrap().take().expect("media route sent no request")
}

fn speech_voice(provider: &str) -> Option<String> {
    match provider {
        "cartesia" => Some(SPEECH_CARTESIA_VOICE.to_string()),
        "elevenlabs" => Some(SPEECH_ELEVENLABS_VOICE.to_string()),
        _ => None,
    }
}

fn request(prompt: &str) -> LlmRequest {
    serde_json::from_value(json!({
        "system": [],
        "messages": [{ "role": "user", "content": [{ "type": "text", "text": prompt }] }],
        "tools": []
    }))
    .expect("request")
}

fn compare_prepared(expected: &Prepared, request: &PreparedHttp) -> Option<String> {
    if request.method != expected.method {
        return Some(format!("method {:?} != {:?}", request.method, expected.method));
    }
    if request.url != expected.url {
        return Some(format!("url {:?} != {:?}", request.url, expected.url));
    }
    let (headers, body) = normalize(request);
    if headers != expected.headers {
        return Some(format!("headers {:?} != {:?}", headers, expected.headers));
    }
    let encoded = BASE64_STANDARD.encode(body);
    if encoded != expected.body {
        return Some(format!("body {encoded} != {}", expected.body));
    }
    None
}

fn normalize(request: &PreparedHttp) -> (BTreeMap<String, String>, Vec<u8>) {
    let mut headers: BTreeMap<String, String> =
        request.headers.iter().map(|(name, value)| (name.to_lowercase(), value.clone())).collect();
    let boundary = headers.get("content-type").and_then(|value| boundary(value));
    let Some(boundary) = boundary else {
        return (headers, request.body.to_vec());
    };
    if let Some(content_type) = headers.get_mut("content-type") {
        *content_type = content_type.replace(&boundary, FIXTURE_BOUNDARY);
    }
    let body = replace(&request.body, boundary.as_bytes(), FIXTURE_BOUNDARY.as_bytes());
    (headers, body)
}

fn boundary(content_type: &str) -> Option<String> {
    if content_type.split(';').next()?.trim().to_lowercase() != "multipart/form-data" {
        return None;
    }
    let rest = &content_type[content_type.find("boundary=")? + "boundary=".len()..];
    Some(rest.trim_matches('"').split(';').next()?.trim().to_string())
}

fn replace(bytes: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let mut result = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index..].starts_with(from) {
            result.extend_from_slice(to);
            index += from.len();
        } else {
            result.push(bytes[index]);
            index += 1;
        }
    }
    result
}

#[test]
fn registered_specifiers_match_the_corpus() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/provider-entries.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let document: Document = serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let corpus: BTreeSet<&str> = document.cases.iter().map(|case| case.specifier.as_str()).collect();
    let registered = ProviderPackages::builtin().specifiers();
    assert_eq!(registered, corpus, "registered provider specifiers");
}
