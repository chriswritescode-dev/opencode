use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures_util::stream;

use base64::Engine;

use opencode_ai::ExecutionContext;
use opencode_ai::MapEnv;
use opencode_ai::clock::FixedClock;
use opencode_ai::id::SequenceIds;
use opencode_ai::media::route::MediaEndpoint;
use opencode_ai::modalities::decision::{DecisionPart, DecisionQuestion, DecisionRequest};
use opencode_ai::protocols::google_decisions;
use opencode_ai::protocols::system_one::{self, SystemOne};
use opencode_ai::route::auth::Auth;
use opencode_ai::route::decision::{DecisionRoute, DecisionRouteInput};
use opencode_ai::route::executor::{
    BoxFuture, ByteStream, HttpResponse, HttpSender, Middleware, PreparedHttp, RequestExecutor,
};
use opencode_ai::schema::{AiError, AiErrorReason, MediaAsset, MediaSource, StringMap};
use serde_json::{Value, json};

const PNG: &[u8] = &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x01];
const MEDIA_URL: &str = "https://bucket.s3.amazonaws.com/x.png";

struct UrlMediaSender {
    gets: Mutex<Vec<PreparedHttp>>,
    posts: Mutex<Vec<Value>>,
}

impl UrlMediaSender {
    fn new() -> Arc<Self> {
        Arc::new(Self { gets: Mutex::new(Vec::new()), posts: Mutex::new(Vec::new()) })
    }
}

impl HttpSender for UrlMediaSender {
    fn send(&self, request: PreparedHttp) -> BoxFuture<Result<HttpResponse, AiError>> {
        if request.method == "GET" {
            self.gets.lock().expect("recorded gets").push(request);
            let headers = vec![("content-type".to_string(), "application/octet-stream".to_string())];
            let body: ByteStream = Box::pin(stream::iter(vec![Ok(Bytes::from_static(PNG))]));
            return Box::pin(async move { Ok(HttpResponse::new(200, headers, body)) });
        }
        let posted: Value = serde_json::from_slice(&request.body).expect("decision request body");
        self.posts.lock().expect("recorded posts").push(posted);
        let payload = serde_json::to_vec(&json!({
            "model": "jev-latest",
            "answers": { "refund": { "type": "noul", "noul": 0.9 } }
        }))
        .expect("encoded answer");
        let body: ByteStream = Box::pin(stream::iter(vec![Ok(Bytes::from(payload))]));
        Box::pin(async move { Ok(HttpResponse::new(200, Vec::new(), body)) })
    }
}

fn route() -> DecisionRoute {
    DecisionRoute::make(DecisionRouteInput {
        id: system_one::ID.to_string(),
        provider: "typesafe-ai".to_string(),
        protocol: Arc::new(SystemOne),
        endpoint: MediaEndpoint::path(system_one::PATH).with_base_url(system_one::DEFAULT_BASE_URL),
        auth: Auth::headers([("authorization".to_string(), "Bearer fixture".to_string())]),
        headers: None,
        provider_metadata_key: None,
        options: None,
    })
}

fn context() -> ExecutionContext {
    ExecutionContext::new(Arc::new(FixedClock::new(0)), Arc::new(SequenceIds::new()), Arc::new(MapEnv::default()))
}

fn executor(sender: Arc<UrlMediaSender>) -> RequestExecutor {
    RequestExecutor::new(sender, None, Arc::new(FixedClock::new(0)))
}

fn url_media(media_type: Option<&str>, headers: Option<StringMap>) -> MediaAsset {
    MediaAsset {
        source: MediaSource::Url {
            url: MEDIA_URL.to_string(),
            media_type: media_type.map(str::to_string),
            expires_at: None,
        },
        info: None,
        provider_metadata: None,
        headers,
    }
}

fn decision_request(media: MediaAsset) -> DecisionRequest {
    DecisionRequest {
        model: route().model("jev-latest", None, None),
        input: vec![DecisionPart::Media { media, filename: None }],
        questions: vec![DecisionQuestion::Predicate {
            name: "refund".to_string(),
            instructions: "Is the customer asking for a refund?".to_string(),
            criteria: None,
        }],
        provider_options: None,
        headers: None,
        timeout: None,
        http: None,
    }
}

async fn decision_body(media: MediaAsset) -> Value {
    let sender = UrlMediaSender::new();
    let response = route().generate(decision_request(media), &executor(sender.clone()), &context()).await;
    assert!(response.is_ok(), "decision generate failed: {:?}", response.err().map(|error| error.to_string()));
    let posts = sender.posts.lock().expect("recorded posts");
    assert_eq!(posts.len(), 1, "one decision request was posted");
    posts[0].clone()
}

#[tokio::test]
async fn decision_url_media_ignores_the_response_content_type_and_sniffs_the_bytes() {
    let body = decision_body(url_media(None, None)).await;
    assert_eq!(body["assets"]["<image-1>"]["type"], json!("image"));
    assert_eq!(body["assets"]["<image-1>"]["format"], json!("png"));
}

#[tokio::test]
async fn decision_url_media_keeps_a_declared_media_type() {
    let body = decision_body(url_media(Some("image/jpeg"), None)).await;
    assert_eq!(body["assets"]["<image-1>"]["type"], json!("image"));
    assert_eq!(body["assets"]["<image-1>"]["format"], json!("jpeg"));
}

#[tokio::test]
async fn decision_url_media_downloads_with_the_asset_headers_and_without_the_caller_middleware() {
    let sender = UrlMediaSender::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = seen.clone();
    let middleware: Middleware = Arc::new(move |request, next| {
        recorder.lock().expect("recorded middleware").push(request.url.clone());
        next(request)
    });
    let executor = executor(sender.clone()).with_middleware(middleware);
    let headers = StringMap::from_pairs([("x-asset-token".to_string(), "asset-secret".to_string())]);

    let response = route().generate(decision_request(url_media(None, Some(headers))), &executor, &context()).await;
    assert!(response.is_ok(), "decision generate failed: {:?}", response.err().map(|error| error.to_string()));

    let gets = sender.gets.lock().expect("recorded gets");
    assert_eq!(gets.len(), 1);
    assert_eq!(gets[0].url, MEDIA_URL);
    assert_eq!(
        gets[0].headers.iter().find(|(name, _)| name == "x-asset-token").map(|(_, value)| value.as_str()),
        Some("asset-secret")
    );
    assert_eq!(*seen.lock().expect("recorded middleware"), vec!["https://api.typesafe.ai/v1/systemone".to_string()]);
}

#[tokio::test]
async fn decision_provider_refs_fail_before_reaching_the_network() {
    let sender = UrlMediaSender::new();
    let request = decision_request(MediaAsset {
        source: MediaSource::Ref {
            provider: "google".to_string(),
            id: "files/x".to_string(),
            media_type: Some("image/png".to_string()),
        },
        info: None,
        provider_metadata: None,
        headers: None,
    });

    let error = route().generate(request, &executor(sender.clone()), &context()).await.expect_err("provider ref");
    assert!(matches!(error.reason, AiErrorReason::InvalidRequest { .. }));
    assert_eq!(
        error.message(),
        "System One requires inline media (bytes or base64); ref sources must be materialized first"
    );
    assert!(sender.gets.lock().expect("recorded gets").is_empty());
    assert!(sender.posts.lock().expect("recorded posts").is_empty());
}

#[tokio::test]
async fn system_one_keeps_a_single_media_item_in_a_state_list() {
    let sender = UrlMediaSender::new();
    let media = MediaAsset {
        source: MediaSource::Base64 {
            data: base64::prelude::BASE64_STANDARD.encode(PNG),
            media_type: "image/png".to_string(),
        },
        info: None,
        provider_metadata: None,
        headers: None,
    };
    let mut request = decision_request(media);
    request.model = route().model("jev-latest", Some(json!({ "mediaInput": "state" })), None);

    let response = route().generate(request, &executor(sender.clone()), &context()).await;
    assert!(response.is_ok(), "decision generate failed: {:?}", response.err().map(|error| error.to_string()));

    let posts = sender.posts.lock().expect("recorded posts");
    let state = posts[0]["state"].as_array().expect("state stays a list");
    assert_eq!(state.len(), 1);
}

fn google_route() -> DecisionRoute {
    DecisionRoute::make(DecisionRouteInput {
        id: google_decisions::ID.to_string(),
        provider: "google".to_string(),
        protocol: Arc::new(google_decisions::GoogleDecisions),
        endpoint: MediaEndpoint::path(google_decisions::PATH).with_base_url(google_decisions::DEFAULT_BASE_URL),
        auth: Auth::headers([("authorization".to_string(), "Bearer fixture".to_string())]),
        headers: None,
        provider_metadata_key: None,
        options: None,
    })
}

#[tokio::test]
async fn google_decisions_keeps_a_url_with_asset_headers_out_of_uri() {
    let sender = UrlMediaSender::new();
    let headers = StringMap::from_pairs([("x-asset-token".to_string(), "asset-secret".to_string())]);
    let route = google_route();
    let mut request = decision_request(url_media(Some("image/png"), Some(headers)));
    request.model = route.model("gemini-2.5-flash", None, None);

    let error = route
        .generate(request, &executor(sender.clone()), &context())
        .await
        .expect_err("a credentialed URL cannot be forwarded as a URI");
    assert!(matches!(error.reason, AiErrorReason::InvalidRequest { .. }));
    assert_eq!(
        error.message(),
        "Google Decisions accepts inline media, public HTTPS URLs and Google Files API URIs; materialize url media first"
    );
    assert!(sender.gets.lock().expect("recorded gets").is_empty());
}
