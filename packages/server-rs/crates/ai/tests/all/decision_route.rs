use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures_util::stream;

use opencode_ai::ExecutionContext;
use opencode_ai::MapEnv;
use opencode_ai::clock::FixedClock;
use opencode_ai::id::SequenceIds;
use opencode_ai::media::route::MediaEndpoint;
use opencode_ai::modalities::decision::{DecisionPart, DecisionQuestion, DecisionRequest};
use opencode_ai::protocols::system_one::{self, SystemOne};
use opencode_ai::route::auth::Auth;
use opencode_ai::route::decision::{DecisionRoute, DecisionRouteInput};
use opencode_ai::route::executor::{BoxFuture, ByteStream, HttpResponse, HttpSender, PreparedHttp, RequestExecutor};
use opencode_ai::schema::{AiError, MediaAsset, MediaSource};
use serde_json::{Value, json};

const PNG: &[u8] = &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x01];

struct UrlMediaSender {
    posts: Mutex<Vec<Value>>,
}

impl HttpSender for UrlMediaSender {
    fn send(&self, request: PreparedHttp) -> BoxFuture<Result<HttpResponse, AiError>> {
        if request.method == "GET" {
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

async fn decision_body(media_type: Option<&str>) -> Value {
    let sender = Arc::new(UrlMediaSender { posts: Mutex::new(Vec::new()) });
    let clock = Arc::new(FixedClock::new(0));
    let context = ExecutionContext::new(clock.clone(), Arc::new(SequenceIds::new()), Arc::new(MapEnv::default()));
    let executor = RequestExecutor::new(sender.clone(), None, clock);
    let request = DecisionRequest {
        model: route().model("jev-latest", None, None),
        input: vec![DecisionPart::Media {
            media: MediaAsset {
                source: MediaSource::Url {
                    url: "https://bucket.s3.amazonaws.com/x.png".to_string(),
                    media_type: media_type.map(str::to_string),
                    expires_at: None,
                },
                info: None,
                provider_metadata: None,
                headers: None,
            },
            filename: None,
        }],
        questions: vec![DecisionQuestion::Predicate {
            name: "refund".to_string(),
            instructions: "Is the customer asking for a refund?".to_string(),
            criteria: None,
        }],
        provider_options: None,
        headers: None,
        timeout: None,
        http: None,
    };
    let response = route().generate(request, &executor, &context).await;
    assert!(response.is_ok(), "decision generate failed: {:?}", response.err().map(|error| error.to_string()));
    let posts = sender.posts.lock().expect("recorded posts");
    assert_eq!(posts.len(), 1, "one decision request was posted");
    posts[0].clone()
}

#[tokio::test]
async fn decision_url_media_ignores_the_response_content_type_and_sniffs_the_bytes() {
    let body = decision_body(None).await;
    assert_eq!(body["assets"]["<image-1>"]["type"], json!("image"));
    assert_eq!(body["assets"]["<image-1>"]["format"], json!("png"));
}

#[tokio::test]
async fn decision_url_media_keeps_a_declared_media_type() {
    let body = decision_body(Some("image/jpeg")).await;
    assert_eq!(body["assets"]["<image-1>"]["type"], json!("image"));
    assert_eq!(body["assets"]["<image-1>"]["format"], json!("jpeg"));
}
