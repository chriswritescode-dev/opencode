use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures_util::{StreamExt, stream};
use serde_json::json;

use opencode_ai::ExecutionContext;
use opencode_ai::LlmClient;
use opencode_ai::MapEnv;
use opencode_ai::clock::FixedClock;
use opencode_ai::id::SequenceIds;
use opencode_ai::providers::google_adc::{CredentialFiles, GoogleAdc, expand_cloud_sdk_config};
use opencode_ai::providers::{ProviderPackages, RuntimeModel, Settings};
use opencode_ai::route::auth::CredentialSource;
use opencode_ai::route::compile::StreamOptions;
use opencode_ai::route::executor::{BoxFuture, HttpResponse, HttpSender, PreparedHttp, RequestExecutor};
use opencode_ai::schema::{AiError, AiErrorReason};

use crate::compile::request;
use crate::corpus::corpus_dir;

const CLOCK_EPOCH_MS: u64 = 1_767_323_045_000;

static COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Corpus {
    clock_epoch_ms: u64,
    cases: Vec<Case>,
}

#[derive(serde::Deserialize)]
struct Case {
    name: String,
    files: BTreeMap<String, String>,
    env: BTreeMap<String, String>,
    steps: Vec<Step>,
}

#[derive(serde::Deserialize)]
struct Step {
    requests: Vec<Recorded>,
    token: Option<String>,
    error: Option<String>,
}

#[derive(Clone, serde::Deserialize)]
struct Recorded {
    method: String,
    url: String,
    headers: BTreeMap<String, String>,
    body: String,
    response: Response,
}

#[derive(Clone, serde::Deserialize)]
struct Response {
    status: u16,
    headers: BTreeMap<String, String>,
    body: String,
}

#[derive(Debug)]
struct ScriptedFiles {
    files: BTreeMap<String, String>,
}

impl CredentialFiles for ScriptedFiles {
    fn exists(&self, path: &str) -> bool {
        self.files.contains_key(&normalize(path))
    }

    fn read(&self, path: &str) -> Option<String> {
        self.files.get(&normalize(path)).cloned()
    }
}

struct ScriptedSender {
    script: Mutex<VecDeque<Recorded>>,
    sent: Mutex<Vec<PreparedHttp>>,
}

impl ScriptedSender {
    fn new(script: Vec<Recorded>) -> Self {
        Self { script: Mutex::new(script.into()), sent: Mutex::new(Vec::new()) }
    }
}

impl HttpSender for ScriptedSender {
    fn send(&self, request: PreparedHttp) -> BoxFuture<Result<HttpResponse, AiError>> {
        let recorded = self.script.lock().unwrap().pop_front().expect("unexpected ADC request");
        self.sent.lock().unwrap().push(request);
        let status = recorded.response.status;
        let headers =
            recorded.response.headers.iter().map(|(name, value)| (name.clone(), value.clone())).collect::<Vec<_>>();
        let body = recorded.response.body;
        Box::pin(async move {
            Ok(HttpResponse::new(status, headers, Box::pin(stream::once(async { Ok(Bytes::from(body)) }))))
        })
    }
}

fn normalize(path: &str) -> String {
    path.replace('\\', "/")
}

fn scripted(status: u16, headers: &[(&str, &str)], body: &str) -> Recorded {
    Recorded {
        method: String::new(),
        url: String::new(),
        headers: BTreeMap::new(),
        body: String::new(),
        response: Response {
            status,
            headers: headers.iter().map(|(name, value)| ((*name).to_string(), (*value).to_string())).collect(),
            body: body.to_string(),
        },
    }
}

struct PendingSender;

impl HttpSender for PendingSender {
    fn send(&self, _request: PreparedHttp) -> BoxFuture<Result<HttpResponse, AiError>> {
        Box::pin(futures_util::future::pending())
    }
}

#[tokio::test(start_paused = true)]
async fn google_adc_metadata_requests_time_out() {
    let sender: Arc<dyn HttpSender> = Arc::new(PendingSender);
    let probe = ExecutionContext::new(
        Arc::new(FixedClock::new(CLOCK_EPOCH_MS)),
        Arc::new(SequenceIds::new()),
        Arc::new(MapEnv::new(vec![("HOME".to_string(), "/adc/home".to_string())])),
    );
    let source = GoogleAdc::with_files(None, Arc::new(ScriptedFiles { files: BTreeMap::new() }));
    let started = tokio::time::Instant::now();
    let outcome = source.prepare(&probe, &sender).await;
    assert_eq!(started.elapsed(), std::time::Duration::from_millis(3000), "metadata detection timeout");
    assert_missing_adc(outcome);

    let assumed = ExecutionContext::new(
        Arc::new(FixedClock::new(CLOCK_EPOCH_MS)),
        Arc::new(SequenceIds::new()),
        Arc::new(MapEnv::new(vec![("METADATA_SERVER_DETECTION".to_string(), "assume-present".to_string())])),
    );
    let source = GoogleAdc::with_files(None, Arc::new(ScriptedFiles { files: BTreeMap::new() }));
    let started = tokio::time::Instant::now();
    let outcome = source.prepare(&assumed, &sender).await;
    assert_eq!(started.elapsed(), std::time::Duration::from_millis(3000), "metadata token timeout");
    assert_missing_adc(outcome);
}

fn assert_missing_adc(outcome: Result<(), AiError>) {
    match outcome {
        Err(AiError { reason: AiErrorReason::Authentication { message, .. }, .. }) => {
            assert_eq!(message, "Missing auth credential: Google Application Default Credentials");
        }
        other => panic!("expected the ADC authentication error, got {other:?}"),
    }
}

#[test]
fn cloud_sdk_config_expands_the_home_prefix() {
    assert_eq!(
        expand_cloud_sdk_config("~/sdk", Some("/adc/home")),
        std::path::Path::new("/adc/home").join("sdk").to_string_lossy()
    );
    assert_eq!(expand_cloud_sdk_config("~", Some("/adc/home")), "/adc/home");
    assert_eq!(expand_cloud_sdk_config("/adc/sdk", Some("/adc/home")), "/adc/sdk");
    assert_eq!(expand_cloud_sdk_config("~/sdk", None), "~/sdk");
}

#[tokio::test]
async fn google_adc_replays_the_captured_cases() {
    let text = std::fs::read_to_string(corpus_dir().join("google-adc.json")).expect("read google-adc corpus");
    let corpus: Corpus = serde_json::from_str(&text).expect("parse google-adc corpus");
    assert_eq!(corpus.cases.len(), 11, "google-adc corpus cases");
    assert_eq!(corpus.cases.iter().map(|case| case.steps.len()).sum::<usize>(), 15, "google-adc corpus steps");
    assert_eq!(
        corpus.cases.iter().flat_map(|case| &case.steps).map(|step| step.requests.len()).sum::<usize>(),
        10,
        "google-adc corpus requests"
    );

    for case in &corpus.cases {
        let files = case.files.iter().map(|(path, content)| (normalize(path), content.clone())).collect();
        let env = MapEnv::new(case.env.clone());
        let context = ExecutionContext::new(
            Arc::new(FixedClock::new(corpus.clock_epoch_ms)),
            Arc::new(SequenceIds::new()),
            Arc::new(env.clone()),
        );
        let source = GoogleAdc::with_files(None, Arc::new(ScriptedFiles { files }));
        let sender =
            Arc::new(ScriptedSender::new(case.steps.iter().flat_map(|step| step.requests.iter().cloned()).collect()));
        let sender_dyn: Arc<dyn HttpSender> = sender.clone();
        let mut sent_before = 0;
        for step in &case.steps {
            let outcome = match source.prepare(&context, &sender_dyn).await {
                Ok(()) => source.load(&env),
                Err(error) => Err(error),
            };
            match (step.token.as_deref(), step.error.as_deref()) {
                (Some(token), None) => match outcome {
                    Ok(actual) => assert_eq!(actual, token, "{}", case.name),
                    Err(error) => panic!("{}: expected token {token}, got {error:?}", case.name),
                },
                (None, Some(message)) => match outcome {
                    Ok(token) => panic!("{}: expected error {message}, got token {token}", case.name),
                    Err(error) => match error.reason {
                        AiErrorReason::Authentication { message: actual, .. } => {
                            assert_eq!(actual, message, "{}", case.name);
                        }
                        other => panic!("{}: expected authentication error, got {other:?}", case.name),
                    },
                },
                _ => panic!("{}: step has neither a token nor an error", case.name),
            }
            {
                let sent = sender.sent.lock().unwrap();
                assert_eq!(sent.len() - sent_before, step.requests.len(), "{}: request count", case.name);
                for (actual, expected) in sent[sent_before..].iter().zip(&step.requests) {
                    assert_eq!(actual.method, expected.method, "{}: method", case.name);
                    assert_eq!(actual.url, expected.url, "{}: url", case.name);
                    let sent_body = std::str::from_utf8(&actual.body).expect("utf-8 request body");
                    assert_eq!(sent_body, expected.body, "{}: body", case.name);
                    let sent_headers = actual
                        .headers
                        .iter()
                        .map(|(name, value)| (name.to_lowercase(), value.clone()))
                        .collect::<BTreeMap<_, _>>();
                    let recorded_headers = expected
                        .headers
                        .iter()
                        .map(|(name, value)| (name.to_lowercase(), value.clone()))
                        .collect::<BTreeMap<_, _>>();
                    assert_eq!(sent_headers, recorded_headers, "{}: headers", case.name);
                }
            }
            sent_before += step.requests.len();
        }
        assert!(sender.script.lock().unwrap().is_empty(), "{}: unused scripted requests", case.name);
    }
}

#[tokio::test]
async fn google_adc_authorizes_the_vertex_chat_route() {
    let path = std::env::temp_dir().join(format!(
        "opencode-adc-{}-{}.json",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::write(
        &path,
        r#"{"type":"authorized_user","client_id":"adc-client","client_secret":"adc-secret","refresh_token":"adc-refresh"}"#,
    )
    .expect("write credential file");
    let env = MapEnv::new(vec![("GOOGLE_APPLICATION_CREDENTIALS".to_string(), path.to_string_lossy().into_owned())]);
    let settings =
        Settings::new(json!({ "baseURL": "https://vertex.example.test" }).as_object().expect("settings").clone());
    let runtime = ProviderPackages::builtin()
        .model("@opencode/ai/providers/google-vertex/chat", "gemini-chat", &settings, &env)
        .expect("vertex chat model");
    let RuntimeModel::Language(model) = runtime else {
        panic!("google-vertex/chat: expected a language model");
    };
    let sender = Arc::new(ScriptedSender::new(vec![
        scripted(
            200,
            &[("content-type", "application/json")],
            "{\"access_token\":\"adc-e2e-token\",\"expires_in\":3600,\"token_type\":\"Bearer\"}",
        ),
        scripted(200, &[("content-type", "text/event-stream")], "data: [DONE]\n\n"),
    ]));
    let clock = Arc::new(FixedClock::new(CLOCK_EPOCH_MS));
    let context = ExecutionContext::new(clock.clone(), Arc::new(SequenceIds::new()), Arc::new(env));
    let client = LlmClient::new(RequestExecutor::new(sender.clone(), None, clock), context);
    let _events: Vec<_> = client.stream(&model, request("Hi"), StreamOptions::default()).collect().await;

    let sent = sender.sent.lock().unwrap();
    assert_eq!(sent.len(), 2, "ADC token request and model request");
    assert_eq!(sent[0].method, "POST");
    assert_eq!(sent[0].url, "https://oauth2.googleapis.com/token");
    assert_eq!(sent[1].url, "https://vertex.example.test/chat/completions");
    let authorization =
        sent[1].headers.iter().find(|(name, _)| name == "authorization").map(|(_, value)| value.as_str());
    assert_eq!(authorization, Some("Bearer adc-e2e-token"));
    drop(sent);

    std::fs::remove_file(&path).expect("remove credential file");
}
