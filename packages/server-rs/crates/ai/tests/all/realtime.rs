use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures_util::stream;
use serde::Deserialize;
use serde_json::Value;

use opencode_ai::ExecutionContext;
use opencode_ai::MapEnv;
use opencode_ai::clock::FixedClock;
use opencode_ai::id::SequenceIds;
use opencode_ai::modalities::realtime::{RealtimeConnectInput, connect};
use opencode_ai::route::executor::{BoxFuture, ByteStream, HttpResponse, HttpSender, PreparedHttp, RequestExecutor};
use opencode_ai::schema::AiError;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Corpus {
    clock_epoch_ms: u64,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    input: RealtimeConnectInput,
    requests: Vec<RecordedRequest>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<RecordedError>,
}

#[derive(Deserialize)]
struct RecordedRequest {
    method: String,
    url: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    body: String,
    #[serde(default)]
    boundary: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RecordedError {
    reason: Value,
    retryable: bool,
}

struct ScriptedSender {
    remaining: Mutex<VecDeque<RecordedRequest>>,
}

impl ScriptedSender {
    fn new(requests: Vec<RecordedRequest>) -> Arc<Self> {
        Arc::new(Self { remaining: Mutex::new(requests.into()) })
    }

    fn assert_consumed(&self) {
        let remaining = self.remaining.lock().expect("scripted requests");
        assert!(remaining.is_empty(), "{} recorded realtime requests were not consumed", remaining.len());
    }
}

impl HttpSender for ScriptedSender {
    fn send(&self, request: PreparedHttp) -> BoxFuture<Result<HttpResponse, AiError>> {
        let recorded = self.remaining.lock().expect("scripted requests").pop_front();
        let Some(recorded) = recorded else {
            panic!("unexpected realtime request: {} {}", request.method, request.url);
        };
        assert_prepared(&recorded, &request);
        let (headers, body) = reply_for(&request.url);
        let body: ByteStream = Box::pin(stream::iter(vec![Ok(Bytes::from(body))]));
        Box::pin(async move { Ok(HttpResponse::new(200, headers, body)) })
    }
}

fn assert_prepared(recorded: &RecordedRequest, actual: &PreparedHttp) {
    if let Some(boundary) = &recorded.boundary {
        assert!(recorded.body.starts_with(&format!("--{boundary}\r\n")), "recorded boundary for {}", recorded.url);
    }
    assert_eq!(actual.method, recorded.method);
    assert_eq!(actual.url, recorded.url);
    assert_eq!(
        normalized_headers(actual.headers.clone()),
        normalized_headers(recorded.headers.clone()),
        "headers for {}",
        recorded.url
    );
    assert_eq!(
        normalized_body(&String::from_utf8_lossy(&actual.body)),
        normalized_body(&recorded.body),
        "body for {}",
        recorded.url
    );
}

fn normalized_headers(headers: impl IntoIterator<Item = (String, String)>) -> BTreeMap<String, String> {
    headers.into_iter().map(|(name, value)| (name.to_lowercase(), normalized_content_type(&value))).collect()
}

fn normalized_content_type(value: &str) -> String {
    match value.split_once("boundary=") {
        Some((prefix, _)) => format!("{prefix}boundary=BOUNDARY"),
        None => value.to_string(),
    }
}

fn normalized_body(body: &str) -> String {
    let Some(first_line) = body.split("\r\n").next() else { return body.to_string() };
    let Some(boundary) = first_line.strip_prefix("--") else { return body.to_string() };
    body.replace(boundary, "BOUNDARY")
}

fn reply_for(url: &str) -> (Vec<(String, String)>, Vec<u8>) {
    let json = vec![("content-type".to_string(), "application/json".to_string())];
    match url {
        "https://api.openai.com/v1/live/sessions" => {
            (json, br#"{"session":{"id":"s1"},"transport":{"sdp":"v=0\r\no=live-answer\r\n"}}"#.to_vec())
        }
        "https://api.openai.com/v1/realtime/calls?dcid=4" => (Vec::new(), b"v=0\r\no=rtc-answer\r\n".to_vec()),
        "https://api.openai.com/v1/realtime/client_secrets" => {
            (json, br#"{"value":"ek_1","expires_at":1760000060}"#.to_vec())
        }
        "https://api.x.ai/v1/realtime/client_secrets" => {
            (json, br#"{"value":"ek_2","expires_at":1760000120}"#.to_vec())
        }
        "https://generativelanguage.googleapis.com/v1beta/auth_tokens" => {
            (json, br#"{"name":"auth_tokens/ek_3"}"#.to_vec())
        }
        other => panic!("unexpected realtime request url: {other}"),
    }
}

#[tokio::test]
async fn realtime_connect_matches_corpus() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/realtime.json");
    let corpus: Corpus = serde_json::from_str(
        &std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display())),
    )
    .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    assert_eq!(corpus.cases.len(), 8);
    assert_eq!(corpus.clock_epoch_ms, 1760000000000);

    for case in corpus.cases {
        let Case { name, input, requests, result, error } = case;
        let sender = ScriptedSender::new(requests);
        let clock = Arc::new(FixedClock::new(corpus.clock_epoch_ms));
        let context = ExecutionContext::new(clock.clone(), Arc::new(SequenceIds::new()), Arc::new(MapEnv::default()));
        let executor = RequestExecutor::new(sender.clone(), None, clock);
        let actual = connect(input, &executor, &context).await;
        sender.assert_consumed();
        match (actual, result, error) {
            (Ok(transport), Some(expected), None) => {
                assert_eq!(serde_json::to_value(&transport).expect("encoded transport"), expected, "{name}");
            }
            (Err(error), None, Some(expected)) => {
                assert_eq!(serde_json::to_value(&error.reason).expect("encoded reason"), expected.reason, "{name}");
                assert_eq!(error.is_retryable(), expected.retryable, "{name}");
            }
            (actual, result, error) => {
                panic!("{name}: unexpected outcome {actual:?}, recorded result {result:?}, recorded error {error:?}")
            }
        }
    }
}
