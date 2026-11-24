use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::prelude::{BASE64_STANDARD, Engine};
use bytes::Bytes;
use futures_util::stream;
use serde_json::{Number, Value, json};

use opencode_ai::ExecutionContext;
use opencode_ai::MapEnv;
use opencode_ai::Sleeper;
use opencode_ai::clock::{Clock, FixedClock};
use opencode_ai::id::SequenceIds;
use opencode_ai::media::MediaRoute;
use opencode_ai::media::generation::{AwaitOptions, Generation, GenerationRoute, Poll, Reason, Snapshot, Status};
use opencode_ai::media::multipart::{AppendFieldsOptions, Form, RepeatArrays, append_fields};
use opencode_ai::media::route::{QueuedRoute, RouteKind, StreamedProtocol, read_with_retry};
use opencode_ai::modalities::image::{
    ImageClient, ImageEvent, ImageModel, ImageRequest, ImageRequestInput, ImageResponse,
};
use opencode_ai::modalities::realtime::RealtimeConnectInput;
use opencode_ai::modalities::speech::{SpeechClient, SpeechModel, SpeechRequest, SpeechRequestInput, SpeechResponse};
use opencode_ai::modalities::video::{VideoClient, VideoModel, VideoRequest, seconds};
use opencode_ai::protocols::openai_images::{Frame, OpenAiImages, State};
use opencode_ai::providers::{ProviderPackages, RuntimeModel, Settings};
use opencode_ai::route::executor::{BoxFuture, ByteStream, HttpResponse, HttpSender, PreparedHttp, RequestExecutor};
use opencode_ai::schema::{AiError, AiErrorReason, HttpOptions, MediaAsset, MediaSource, ProviderOptions, StringMap};

#[test]
fn multipart_fixtures_match_bun() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/multipart-fixtures.json");
    let document: Value = serde_json::from_str(
        &std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display())),
    )
    .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let fixtures = document["fixtures"].as_array().unwrap();
    assert!(fixtures.len() >= 6);
    for fixture in fixtures {
        let name = fixture["name"].as_str().unwrap();
        let fields = fixture["fields"].as_object().unwrap().clone();
        let overlay = fixture["overlay"].as_object().cloned();
        let reserved: Vec<String> =
            fixture["reserved"].as_array().unwrap().iter().map(|value| value.as_str().unwrap().to_string()).collect();
        let reserved: Vec<&str> = reserved.iter().map(String::as_str).collect();
        let repeat_arrays = match fixture["repeatArrays"].as_str() {
            None => None,
            Some("key[]") => Some(RepeatArrays::Brackets),
            Some("key") => Some(RepeatArrays::Key),
            Some(other) => panic!("{name}: unknown repeatArrays {other}"),
        };
        let mut form = Form::new();
        append_fields(
            &mut form,
            &fields,
            AppendFieldsOptions { overlay: overlay.as_ref(), reserved: &reserved, repeat_arrays },
        );
        if let Some(file) = fixture["file"].as_object() {
            let bytes = BASE64_STANDARD.decode(file["bytesBase64"].as_str().unwrap()).unwrap();
            form.append_file(
                file["name"].as_str().unwrap(),
                bytes,
                Some(file["filename"].as_str().unwrap().to_string()),
                Some(file["contentType"].as_str().unwrap().to_string()),
            );
        }
        let expected = BASE64_STANDARD.decode(fixture["bodyBase64"].as_str().unwrap()).unwrap();
        let actual = form.serialize(fixture["boundary"].as_str().unwrap());
        assert_eq!(actual, expected, "fixture {name}");
    }
}

#[derive(Default)]
struct AdvancingClock {
    now: AtomicU64,
}

impl Clock for AdvancingClock {
    fn now_ms(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }
}

struct NoSleep {
    clock: Arc<AdvancingClock>,
    durations: Mutex<Vec<Duration>>,
}

impl NoSleep {
    fn new(clock: Arc<AdvancingClock>) -> Self {
        Self { clock, durations: Mutex::new(Vec::new()) }
    }

    fn durations(&self) -> Vec<Duration> {
        self.durations.lock().unwrap().clone()
    }
}

impl Sleeper for NoSleep {
    fn sleep(&self, duration: Duration) -> futures_util::future::BoxFuture<'static, ()> {
        self.durations.lock().unwrap().push(duration);
        self.clock.now.fetch_add(duration.as_millis() as u64, Ordering::SeqCst);
        Box::pin(async {})
    }
}

type Attempt<T> = Arc<dyn Fn() -> BoxFuture<Result<T, AiError>> + Send + Sync>;

fn provider_internal(retry_after_ms: Option<u64>) -> AiError {
    AiError {
        reason: AiErrorReason::ProviderInternal {
            message: "provider failed".to_string(),
            body: None,
            http: None,
            retry_after_ms,
        },
    }
}

fn invalid_request() -> AiError {
    AiError {
        reason: AiErrorReason::InvalidRequest {
            message: "invalid request".to_string(),
            body: None,
            http: None,
            parameter: None,
            classification: None,
        },
    }
}

fn retrying_attempt(counter: Arc<AtomicUsize>, first_retry_after: Option<u64>) -> Attempt<u32> {
    Arc::new(move || {
        let attempt = counter.fetch_add(1, Ordering::SeqCst) + 1;
        let retry_after = if attempt == 1 { first_retry_after } else { None };
        Box::pin(async move { if attempt < 9 { Err(provider_internal(retry_after)) } else { Ok(42) } })
    })
}

#[tokio::test]
async fn read_retry_records_exponential_delays() {
    let clock = Arc::new(AdvancingClock::default());
    let sleeper = Arc::new(NoSleep::new(clock));
    let attempts = Arc::new(AtomicUsize::new(0));
    let result = read_with_retry(sleeper.clone(), retrying_attempt(attempts.clone(), None)).await;
    assert_eq!(result.unwrap(), 42);
    assert_eq!(attempts.load(Ordering::SeqCst), 9);
    assert_eq!(
        sleeper.durations(),
        vec![
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(4),
            Duration::from_secs(8),
            Duration::from_secs(16),
            Duration::from_secs(30),
            Duration::from_secs(30),
            Duration::from_secs(30),
        ]
    );

    let clock = Arc::new(AdvancingClock::default());
    let sleeper = Arc::new(NoSleep::new(clock));
    let attempts = Arc::new(AtomicUsize::new(0));
    let result = read_with_retry(sleeper.clone(), retrying_attempt(attempts.clone(), Some(5_000))).await;
    assert_eq!(result.unwrap(), 42);
    assert_eq!(attempts.load(Ordering::SeqCst), 9);
    assert_eq!(
        sleeper.durations(),
        vec![
            Duration::from_secs(5),
            Duration::from_secs(2),
            Duration::from_secs(4),
            Duration::from_secs(8),
            Duration::from_secs(16),
            Duration::from_secs(30),
            Duration::from_secs(30),
            Duration::from_secs(30),
        ]
    );

    let clock = Arc::new(AdvancingClock::default());
    let sleeper = Arc::new(NoSleep::new(clock));
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = attempts.clone();
    let attempt: Attempt<u32> = Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(invalid_request()) })
    });
    let error = read_with_retry(sleeper.clone(), attempt).await.unwrap_err();
    assert!(matches!(error.reason, AiErrorReason::InvalidRequest { .. }));
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert!(sleeper.durations().is_empty());
}

#[tokio::test]
async fn generation_await_times_out() {
    let clock = Arc::new(AdvancingClock::default());
    let sleeper = Arc::new(NoSleep::new(clock.clone()));
    let route = GenerationRoute::<()> {
        status: Arc::new(|| {
            Box::pin(async {
                Ok(Snapshot { id: "gen".to_string(), status: Status::Running, reason: None, progress: None })
            })
        }),
        result: Arc::new(|| Box::pin(async { Ok(()) })),
        cancel: None,
    };
    let generation = Generation::new(
        route,
        json!(null),
        Snapshot { id: "gen".to_string(), status: Status::Running, reason: None, progress: None },
    );
    let context = ExecutionContext::with_sleeper(
        clock.clone(),
        Arc::new(SequenceIds::new()),
        Arc::new(MapEnv::default()),
        sleeper.clone(),
    );
    let error = generation.await_result(&context, None).await.unwrap_err();
    match error.reason {
        AiErrorReason::Timeout { message, timeout_ms, .. } => {
            assert_eq!(message, "Generation gen did not finish within 10m");
            assert_eq!(timeout_ms, Some(600_000));
        }
        other => panic!("expected timeout, got {other:?}"),
    }
    let durations = sleeper.durations();
    assert!(durations.iter().all(|duration| *duration == Duration::from_secs(5)));
    assert!((119..=121).contains(&durations.len()), "{} sleeps", durations.len());
    assert!((599_000..=601_000).contains(&clock.now_ms()), "{}ms", clock.now_ms());
}

#[tokio::test(start_paused = true)]
async fn generation_await_bounds_a_hung_refresh() {
    let route = GenerationRoute::<()> {
        status: Arc::new(|| Box::pin(futures_util::future::pending::<Result<Snapshot, AiError>>())),
        result: Arc::new(|| Box::pin(async { Ok(()) })),
        cancel: None,
    };
    let generation = Generation::new(
        route,
        json!(null),
        Snapshot { id: "gen".to_string(), status: Status::Running, reason: None, progress: None },
    );
    let context =
        ExecutionContext::new(Arc::new(FixedClock::new(0)), Arc::new(SequenceIds::new()), Arc::new(MapEnv::default()));
    let options = Some(AwaitOptions { poll: Some(Poll { interval: None, timeout: Some(Duration::from_millis(50)) }) });
    let error = generation.await_result(&context, options).await.unwrap_err();
    match error.reason {
        AiErrorReason::Timeout { message, timeout_ms, .. } => {
            assert_eq!(message, "Generation gen did not finish within 50ms");
            assert_eq!(timeout_ms, Some(50));
        }
        other => panic!("expected timeout, got {other:?}"),
    }
}

const VOLCENGINE_VIDEOS: &str = "@opencode/ai/providers/volcengine/videos";
const BYTEPLUS_VIDEOS: &str = "@opencode/ai/providers/byteplus/videos";
const XAI_VIDEOS: &str = "@opencode/ai/providers/xai/videos";
const OPENROUTER_VIDEOS: &str = "@opencode/ai/providers/openrouter/videos";
const MINIMAX_VIDEOS: &str = "@opencode/ai/providers/minimax/videos";
const SEEDANCE_MODEL: &str = "doubao-seedance-2-0-260128";

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

type ReplyHandler = Box<dyn Fn(&PreparedHttp, usize) -> Reply + Send + Sync>;

struct DynamicSender {
    calls: Mutex<Vec<PreparedHttp>>,
    handler: ReplyHandler,
}

impl DynamicSender {
    fn new(handler: impl Fn(&PreparedHttp, usize) -> Reply + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self { calls: Mutex::new(Vec::new()), handler: Box::new(handler) })
    }

    fn recorded(&self) -> Vec<PreparedHttp> {
        self.calls.lock().expect("recorded calls").clone()
    }
}

impl HttpSender for DynamicSender {
    fn send(&self, request: PreparedHttp) -> BoxFuture<Result<HttpResponse, AiError>> {
        let reply = {
            let mut calls = self.calls.lock().expect("recorded calls");
            calls.push(request.clone());
            let nth = calls.iter().filter(|seen| seen.method == request.method && seen.url == request.url).count();
            (self.handler)(&request, nth)
        };
        let status = reply.status;
        let headers = reply.headers;
        let body = byte_stream(reply.body);
        Box::pin(async move { Ok(HttpResponse::new(status, headers, body)) })
    }
}

fn byte_stream(bytes: Vec<u8>) -> ByteStream {
    Box::pin(stream::iter(vec![Ok(Bytes::from(bytes))]))
}

fn json_reply(_request: &PreparedHttp, value: Value) -> Reply {
    Reply {
        status: 200,
        headers: vec![("content-type".to_string(), "application/json".to_string())],
        body: serde_json::to_vec(&value).expect("encoded reply"),
    }
}

fn json_reply_status(request: &PreparedHttp, value: Value, status: u16) -> Reply {
    Reply { status, ..json_reply(request, value) }
}

fn runtime(sender: &Arc<DynamicSender>) -> (RequestExecutor, ExecutionContext) {
    let clock = Arc::new(FixedClock::new(0));
    let context = ExecutionContext::new(clock.clone(), Arc::new(SequenceIds::new()), Arc::new(MapEnv::default()));
    let executor = RequestExecutor::new(sender.clone(), None, clock);
    (executor, context)
}

fn video_model(specifier: &str, id: &str, settings: Value) -> VideoModel {
    let settings = Settings::new(settings.as_object().expect("settings object").clone());
    match ProviderPackages::builtin().model(specifier, id, &settings, &MapEnv::default()) {
        Ok(RuntimeModel::Video(model)) => model,
        Ok(_) => panic!("{specifier} is not a video model"),
        Err(error) => panic!("{specifier}: {}", error.message()),
    }
}

fn video_request(model: &VideoModel, prompt: &str) -> VideoRequest {
    VideoRequest {
        model: model.clone(),
        prompt: prompt.to_string(),
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
    }
}

fn data_url(media_type: &str, data: &str) -> MediaAsset {
    MediaAsset {
        source: MediaSource::Base64 { data: data.to_string(), media_type: media_type.to_string() },
        info: None,
        provider_metadata: None,
        headers: None,
    }
}

fn url_input(url: &str, media_type: Option<&str>) -> MediaAsset {
    MediaAsset {
        source: MediaSource::Url { url: url.to_string(), media_type: media_type.map(str::to_string), expires_at: None },
        info: None,
        provider_metadata: None,
        headers: None,
    }
}

fn provider_ref(provider: &str, id: &str) -> MediaAsset {
    MediaAsset {
        source: MediaSource::Ref { provider: provider.to_string(), id: id.to_string(), media_type: None },
        info: None,
        provider_metadata: None,
        headers: None,
    }
}

fn provider_options(value: Value) -> ProviderOptions {
    value.as_object().expect("provider options").clone()
}

fn header_map(pairs: &[(&str, &str)]) -> StringMap {
    StringMap::from_pairs(pairs.iter().map(|(key, value)| (key.to_string(), value.to_string())))
}

fn header<'a>(request: &'a PreparedHttp, name: &str) -> Option<&'a str> {
    request.headers.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
}

fn request_body(request: &PreparedHttp) -> Value {
    serde_json::from_slice(&request.body).expect("request body is JSON")
}

fn reason_tag(reason: &AiErrorReason) -> String {
    serde_json::to_value(reason).expect("serialized reason")["_tag"].as_str().expect("reason tag").to_string()
}

fn seedance_model() -> VideoModel {
    video_model(VOLCENGINE_VIDEOS, SEEDANCE_MODEL, json!({ "apiKey": "test", "baseURL": "https://ark.test/api/v3" }))
}

#[tokio::test]
async fn seedance_start_maps_frames_and_provider_options() {
    let sender = DynamicSender::new(|request, _| json_reply(request, json!({ "id": "cgt-1" })));
    let (executor, context) = runtime(&sender);
    let model = seedance_model();
    let mut request = video_request(&model, "The kitten yawns at the camera");
    request.start = Some(data_url("image/png", "AQID"));
    request.end = Some(url_input("https://example.test/last.png", None));
    request.duration = Some("5 seconds".to_string());
    request.aspect = Some("16:9".to_string());
    request.resolution = Some("720p".to_string());
    request.sound = Some(false);
    request.provider_options =
        Some(provider_options(json!({ "watermark": true, "return_last_frame": true, "future_option": 1 })));
    request.headers = Some(header_map(&[("x-request", "yes")]));
    request.http = Some(HttpOptions { query: Some(header_map(&[("trace", "1")])), ..HttpOptions::default() });

    let job = VideoClient::start(request, &executor, &context).await.expect("started");
    assert_eq!(job.id, "cgt-1");

    let calls = sender.recorded();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].method, "POST");
    assert_eq!(calls[0].url, "https://ark.test/api/v3/contents/generations/tasks?trace=1");
    assert_eq!(header(&calls[0], "authorization"), Some("Bearer test"));
    assert_eq!(header(&calls[0], "x-request"), Some("yes"));
    assert_eq!(
        request_body(&calls[0]),
        json!({
            "model": "doubao-seedance-2-0-260128",
            "content": [
                { "type": "text", "text": "The kitten yawns at the camera" },
                {
                    "type": "image_url",
                    "image_url": { "url": "data:image/png;base64,AQID" },
                    "role": "first_frame"
                },
                {
                    "type": "image_url",
                    "image_url": { "url": "https://example.test/last.png" },
                    "role": "last_frame"
                }
            ],
            "duration": 5,
            "ratio": "16:9",
            "resolution": "720p",
            "generate_audio": false,
            "watermark": true,
            "return_last_frame": true,
            "future_option": 1
        })
    );
}

#[tokio::test]
async fn seedance_start_maps_references_to_their_roles() {
    let sender = DynamicSender::new(|request, _| json_reply(request, json!({ "id": "cgt-2" })));
    let (executor, context) = runtime(&sender);
    let model = seedance_model();
    let mut request = video_request(&model, "Match @Image1 to the motion of @Video1");
    request.references = vec![
        provider_ref("volcengine", "asset-123"),
        url_input("https://example.test/style.png", Some("image/png")),
        url_input("https://example.test/motion.mp4", Some("video/mp4")),
        url_input("https://example.test/voice.mp3", Some("audio/mpeg")),
    ];

    VideoClient::start(request, &executor, &context).await.expect("started");

    assert_eq!(
        request_body(&sender.recorded()[0]),
        json!({
            "model": "doubao-seedance-2-0-260128",
            "content": [
                { "type": "text", "text": "Match @Image1 to the motion of @Video1" },
                {
                    "type": "image_url",
                    "image_url": { "url": "asset://asset-123" },
                    "role": "reference_image"
                },
                {
                    "type": "image_url",
                    "image_url": { "url": "https://example.test/style.png" },
                    "role": "reference_image"
                },
                {
                    "type": "video_url",
                    "video_url": { "url": "https://example.test/motion.mp4" },
                    "role": "reference_video"
                },
                {
                    "type": "audio_url",
                    "audio_url": { "url": "https://example.test/voice.mp3" },
                    "role": "reference_audio"
                }
            ]
        })
    );
}

#[tokio::test]
async fn seedance_start_sends_auto_aspect_as_adaptive_and_omits_unset_fields() {
    let sender = DynamicSender::new(|request, _| json_reply(request, json!({ "id": "cgt-3" })));
    let (executor, context) = runtime(&sender);
    let model = seedance_model();

    let mut auto = video_request(&model, "x");
    auto.aspect = Some("auto".to_string());
    VideoClient::start(auto, &executor, &context).await.expect("started");
    VideoClient::start(video_request(&model, "x"), &executor, &context).await.expect("started");

    let bodies: Vec<Value> = sender.recorded().iter().map(request_body).collect();
    assert_eq!(
        bodies,
        vec![
            json!({
                "model": "doubao-seedance-2-0-260128",
                "content": [{ "type": "text", "text": "x" }],
                "ratio": "adaptive"
            }),
            json!({
                "model": "doubao-seedance-2-0-260128",
                "content": [{ "type": "text", "text": "x" }]
            })
        ]
    );
}

#[tokio::test]
async fn seedance_start_keeps_frames_before_references() {
    let sender = DynamicSender::new(|request, _| json_reply(request, json!({ "id": "cgt-4" })));
    let (executor, context) = runtime(&sender);
    let model = seedance_model();
    let mut request = video_request(&model, "x");
    request.start = Some(url_input("https://example.test/first.png", None));
    request.references = vec![url_input("https://example.test/ref.png", Some("image/png"))];

    VideoClient::start(request, &executor, &context).await.expect("started");

    let body = request_body(&sender.recorded()[0]);
    let roles: Vec<Option<&str>> =
        body["content"].as_array().expect("content items").iter().map(|item| item["role"].as_str()).collect();
    assert_eq!(roles, vec![None, Some("first_frame"), Some("reference_image")]);
}

#[tokio::test]
async fn seedance_start_adds_a_draft_task_and_omits_an_empty_prompt() {
    let sender = DynamicSender::new(|request, _| json_reply(request, json!({ "id": "cgt-5" })));
    let (executor, context) = runtime(&sender);
    let model = seedance_model();
    let mut request = video_request(&model, "");
    request.provider_options =
        Some(provider_options(json!({ "draft_task": { "id": "cgt-draft" }, "resolution": "720p" })));

    VideoClient::start(request, &executor, &context).await.expect("started");

    assert_eq!(
        request_body(&sender.recorded()[0]),
        json!({
            "model": "doubao-seedance-2-0-260128",
            "content": [{ "type": "draft_task", "draft_task": { "id": "cgt-draft" } }],
            "resolution": "720p"
        })
    );
}

#[tokio::test]
async fn seedance_start_rejects_a_foreign_reference_before_sending() {
    let sender = DynamicSender::new(|_, _| panic!("a foreign reference reached the network"));
    let (executor, context) = runtime(&sender);
    let model = seedance_model();
    let mut request = video_request(&model, "x");
    request.references = vec![provider_ref("xai", "file_1")];

    let error = VideoClient::start(request, &executor, &context).await.expect_err("foreign reference");
    assert_eq!(reason_tag(&error.reason), "InvalidRequest");
    assert!(error.message().contains("volcengine references"), "{}", error.message());
    assert!(sender.recorded().is_empty());
}

#[tokio::test]
async fn seedance_providers_use_their_own_host_and_provider_id() {
    let sender = DynamicSender::new(|request, _| json_reply(request, json!({ "id": "cgt-6" })));
    let (executor, context) = runtime(&sender);
    let byteplus = video_model(BYTEPLUS_VIDEOS, "dreamina-seedance-2-0-260128", json!({ "apiKey": "bp" }));
    let volcengine = video_model(VOLCENGINE_VIDEOS, SEEDANCE_MODEL, json!({ "apiKey": "ark" }));
    assert_eq!(byteplus.provider, "byteplus");
    assert_eq!(volcengine.provider, "volcengine");

    let mut byteplus_request = video_request(&byteplus, "x");
    byteplus_request.references = vec![provider_ref("byteplus", "a1")];
    VideoClient::start(byteplus_request, &executor, &context).await.expect("started");
    VideoClient::start(video_request(&volcengine, "x"), &executor, &context).await.expect("started");

    let calls = sender.recorded();
    assert_eq!(
        calls.iter().map(|call| (call.url.as_str(), header(call, "authorization"))).collect::<Vec<_>>(),
        vec![
            ("https://ark.ap-southeast.bytepluses.com/api/v3/contents/generations/tasks", Some("Bearer bp")),
            ("https://ark.cn-beijing.volces.com/api/v3/contents/generations/tasks", Some("Bearer ark")),
        ]
    );
    assert_eq!(request_body(&calls[0])["content"][1]["image_url"]["url"], json!("asset://a1"));
}

#[tokio::test]
async fn seedance_status_reads_one_status_per_call_from_a_round_tripped_token() {
    let sender = DynamicSender::new(|request, nth| {
        if request.method == "POST" {
            return json_reply(request, json!({ "id": "cgt-1" }));
        }
        match nth {
            1 => json_reply(request, json!({ "id": "cgt-1", "status": "queued" })),
            2 => json_reply(request, json!({ "id": "cgt-1", "status": "running" })),
            _ => json_reply(
                request,
                json!({ "id": "cgt-1", "status": "succeeded", "content": { "video_url": "https://ark-content.test/out.mp4" } }),
            ),
        }
    });
    let (executor, context) = runtime(&sender);
    let model = seedance_model();

    let job = VideoClient::start(video_request(&model, "x"), &executor, &context).await.expect("started");
    let restored: String =
        serde_json::from_value(serde_json::to_value(&job.id).expect("encoded token")).expect("decoded token");
    let mut statuses = Vec::new();
    for _ in 0..3 {
        statuses.push(VideoClient::status(&model, &restored, &executor, &context).await.expect("status read"));
    }
    assert_eq!(
        statuses.iter().map(|status| (status.status, status.reason)).collect::<Vec<_>>(),
        vec![(Status::Running, None), (Status::Running, None), (Status::Completed, None)]
    );
    assert_eq!(
        sender.recorded().iter().map(|call| format!("{} {}", call.method, call.url)).collect::<Vec<_>>(),
        vec![
            "POST https://ark.test/api/v3/contents/generations/tasks",
            "GET https://ark.test/api/v3/contents/generations/tasks/cgt-1",
            "GET https://ark.test/api/v3/contents/generations/tasks/cgt-1",
            "GET https://ark.test/api/v3/contents/generations/tasks/cgt-1",
        ]
    );
}

#[tokio::test]
async fn seedance_status_maps_documented_values_and_keeps_unknown_ones_running() {
    let cases = [
        ("queued", Status::Running, None),
        ("running", Status::Running, None),
        ("succeeded", Status::Completed, None),
        ("failed", Status::Error, Some(Reason::Failed)),
        ("cancelled", Status::Error, Some(Reason::Cancelled)),
        ("expired", Status::Error, Some(Reason::Expired)),
        ("paused", Status::Running, None),
        ("constructor", Status::Running, None),
    ];
    let model = seedance_model();
    for (raw, status, reason) in cases {
        let value = json!({ "id": "cgt-1", "status": raw });
        let sender = DynamicSender::new(move |request, _| json_reply(request, value.clone()));
        let (executor, context) = runtime(&sender);
        let decoded = VideoClient::status(&model, "cgt-1", &executor, &context).await.expect("status read");
        assert_eq!((decoded.status, decoded.reason), (status, reason), "{raw}");
    }
}

#[tokio::test]
async fn seedance_result_returns_a_url_with_usage_and_metadata() {
    let succeeded = json!({
        "id": "cgt-1",
        "model": "doubao-seedance-2-0-260128",
        "status": "succeeded",
        "content": { "video_url": "https://ark-content.test/out.mp4" },
        "usage": { "completion_tokens": 108900, "total_tokens": 108900 },
        "created_at": 1779348818,
        "updated_at": 1779348874,
        "seed": 78674,
        "resolution": "720p",
        "ratio": "16:9",
        "duration": 5,
        "framespersecond": 24,
        "service_tier": "default",
        "generate_audio": true,
        "draft": false
    });
    let sender = DynamicSender::new(move |request, _| json_reply(request, succeeded.clone()));
    let (executor, context) = runtime(&sender);
    let model = seedance_model();

    let response = VideoClient::result(&model, "cgt-1", &executor, &context).await.expect("result");
    assert_eq!(
        serde_json::to_value(&response.videos[0].source).expect("video source"),
        json!({
            "type": "url",
            "url": "https://ark-content.test/out.mp4",
            "mediaType": "video/mp4",
            "expiresAt": (1_779_348_874u64 + 24 * 60 * 60) * 1000
        })
    );
    assert_eq!(
        serde_json::to_value(response.videos[0].info.as_ref().expect("video info")).expect("serialized info"),
        json!({ "durationSeconds": 5 })
    );
    assert_eq!(
        serde_json::to_value(response.usage.as_ref().expect("usage")).expect("serialized usage"),
        json!({ "type": "tokens", "output": 108900, "total": 108900 })
    );
    assert_eq!(
        serde_json::to_value(response.provider_metadata.as_ref().expect("metadata")).expect("serialized metadata"),
        json!({
            "volcengine": {
                "taskId": "cgt-1",
                "model": "doubao-seedance-2-0-260128",
                "seed": 78674,
                "ratio": "16:9",
                "resolution": "720p",
                "duration": 5,
                "framesPerSecond": 24,
                "generateAudio": true,
                "draft": false,
                "serviceTier": "default"
            }
        })
    );
}

#[tokio::test]
async fn seedance_result_derives_duration_from_frames_and_keeps_the_last_frame() {
    let body = json!({
        "status": "succeeded",
        "content": {
            "video_url": "https://ark-content.test/out.mov",
            "last_frame_url": "https://ark-content.test/l.jpg"
        },
        "frames": 57,
        "framespersecond": 24,
        "output_format": "mov"
    });
    let sender = DynamicSender::new(move |request, _| json_reply(request, body.clone()));
    let (executor, context) = runtime(&sender);
    let model = seedance_model();

    let response = VideoClient::result(&model, "cgt-1", &executor, &context).await.expect("result");
    match &response.videos[0].source {
        MediaSource::Url { media_type, expires_at, .. } => {
            assert_eq!(media_type.as_deref(), Some("video/quicktime"));
            assert!(expires_at.as_ref().and_then(Number::as_u64).is_some_and(|expires| expires > 0));
        }
        other => panic!("expected a URL source, got {other:?}"),
    }
    assert_eq!(
        response.videos[0].info.as_ref().and_then(|info| info.duration_seconds.clone()),
        Number::from_f64(57.0 / 24.0)
    );
    let metadata = &response.provider_metadata.as_ref().expect("metadata")["volcengine"];
    assert_eq!(metadata["frames"], json!(57));
    assert_eq!(metadata["outputFormat"], json!("mov"));
    assert_eq!(metadata["lastFrameUrl"], json!("https://ark-content.test/l.jpg"));
}

#[tokio::test]
async fn seedance_result_fails_before_the_task_finishes() {
    let model = seedance_model();
    for status in ["queued", "running"] {
        let value = json!({ "id": "cgt-1", "status": status });
        let reply_value = value.clone();
        let sender = DynamicSender::new(move |request, _| json_reply(request, reply_value.clone()));
        let (executor, context) = runtime(&sender);
        let error = VideoClient::result(&model, "cgt-1", &executor, &context).await.expect_err("unfinished result");
        assert_eq!(reason_tag(&error.reason), "InvalidRequest");
        assert_eq!(
            error.message(),
            "Seedance Video generation cgt-1 has not finished; await it before reading the result"
        );
        assert_eq!(error.body(), Some(serde_json::to_string(&value).expect("body").as_str()));
    }
}

#[tokio::test]
async fn seedance_result_fails_with_the_provider_body() {
    let cases = vec![
        (
            json!({ "status": "failed", "error": { "code": "InvalidParameter.TaskTypeConstraint", "message": "ratio must be adaptive." } }),
            "InvalidRequest",
            "Seedance Video generation failed (InvalidParameter.TaskTypeConstraint): ratio must be adaptive.",
        ),
        (
            json!({ "status": "failed", "error": { "code": "AuthenticationError", "message": "Bad key." } }),
            "Authentication",
            "Seedance Video generation failed (AuthenticationError): Bad key.",
        ),
        (
            json!({ "status": "failed", "error": { "code": "QuotaExceeded", "message": "Too many queued tasks." } }),
            "RateLimit",
            "Seedance Video generation failed (QuotaExceeded): Too many queued tasks.",
        ),
        (
            json!({ "status": "failed", "error": { "code": "InternalServiceError", "message": "Retry later." } }),
            "ProviderInternal",
            "Seedance Video generation failed (InternalServiceError): Retry later.",
        ),
        (
            json!({ "status": "failed", "error": { "code": "constructor", "message": "Future code." } }),
            "ProviderInternal",
            "Seedance Video generation failed (constructor): Future code.",
        ),
        (
            json!({ "status": "failed", "error": { "code": "OutputVideoSensitiveContentDetected", "message": "The output video may be sensitive." } }),
            "ContentPolicy",
            "Seedance Video generation failed (OutputVideoSensitiveContentDetected): The output video may be sensitive.",
        ),
        (
            json!({ "status": "failed", "error": { "code": "InputImageSensitiveContentDetected.PrivacyInformation", "message": "Real person." } }),
            "ContentPolicy",
            "Seedance Video generation failed (InputImageSensitiveContentDetected.PrivacyInformation): Real person.",
        ),
        (json!({ "status": "cancelled" }), "InvalidRequest", "Seedance Video task cgt-1 was cancelled"),
        (json!({ "status": "expired" }), "InvalidRequest", "Seedance Video task cgt-1 expired"),
        (
            json!({ "status": "succeeded", "content": {} }),
            "InvalidProviderOutput",
            "Seedance Video completed without a video URL",
        ),
    ];
    let model = seedance_model();
    for (value, tag, message) in cases {
        let reply_value = value.clone();
        let sender = DynamicSender::new(move |request, _| json_reply(request, reply_value.clone()));
        let (executor, context) = runtime(&sender);
        let error = VideoClient::result(&model, "cgt-1", &executor, &context).await.expect_err("terminal result");
        assert_eq!(reason_tag(&error.reason), tag);
        assert_eq!(error.message(), message);
        assert_eq!(error.body(), Some(serde_json::to_string(&value).expect("body").as_str()));
    }
}

#[tokio::test]
async fn seedance_cancel_deletes_the_task_without_retrying_a_refusal() {
    let sender = DynamicSender::new(|request, nth| match nth {
        1 => json_reply(request, json!({})),
        _ => json_reply_status(request, json!({ "error": { "code": "InvalidParameter", "message": "running" } }), 400),
    });
    let (executor, context) = runtime(&sender);
    let model = seedance_model();

    VideoClient::cancel(&model, "cgt-1", &executor, &context).await.expect("cancelled");
    let error = VideoClient::cancel(&model, "cgt-1", &executor, &context).await.expect_err("refused cancel");
    assert_eq!(reason_tag(&error.reason), "InvalidRequest");
    assert_eq!(
        sender.recorded().iter().map(|call| format!("{} {}", call.method, call.url)).collect::<Vec<_>>(),
        vec![
            "DELETE https://ark.test/api/v3/contents/generations/tasks/cgt-1",
            "DELETE https://ark.test/api/v3/contents/generations/tasks/cgt-1",
        ]
    );
}

#[tokio::test]
async fn xai_status_decodes_progress_and_keeps_unknown_values_running() {
    let sender = DynamicSender::new(|request, nth| match nth {
        1 => json_reply(request, json!({ "status": "pending", "progress": 40 })),
        2 => json_reply(request, json!({ "status": "warming_up" })),
        _ => json_reply(request, json!({ "status": "done", "video": { "url": "https://vidgen.x.ai/out.mp4" } })),
    });
    let (executor, context) = runtime(&sender);
    let model = video_model(
        XAI_VIDEOS,
        "grok-imagine-video-1.5",
        json!({ "apiKey": "test", "baseURL": "https://xai.test/v1" }),
    );

    let pending = VideoClient::status(&model, "req_1", &executor, &context).await.expect("status read");
    assert_eq!((pending.status, pending.progress), (Status::Running, Some(0.4)));
    let unknown = VideoClient::status(&model, "req_1", &executor, &context).await.expect("status read");
    assert_eq!((unknown.status, unknown.progress), (Status::Running, None));
    let done = VideoClient::status(&model, "req_1", &executor, &context).await.expect("status read");
    assert_eq!(done.status, Status::Completed);
}

#[tokio::test]
async fn xai_cancel_is_unsupported_without_a_request() {
    let sender = DynamicSender::new(|_, _| panic!("xAI cancel reached the network"));
    let (executor, context) = runtime(&sender);
    let model = video_model(
        XAI_VIDEOS,
        "grok-imagine-video-1.5",
        json!({ "apiKey": "test", "baseURL": "https://xai.test/v1" }),
    );

    let error = VideoClient::cancel(&model, "req_1", &executor, &context).await.expect_err("unsupported cancel");
    assert_eq!(reason_tag(&error.reason), "UnsupportedOperation");
    assert!(error.message().contains("cancel"), "{}", error.message());
    assert!(sender.recorded().is_empty());
}

#[test]
fn video_duration_seconds_parses_seconds() {
    assert_eq!(seconds("5 seconds"), Some(Number::from(5)));
    assert_eq!(seconds("1 second"), Some(Number::from(1)));
    assert_eq!(seconds("1.5 seconds"), Number::from_f64(1.5));
    assert_eq!(seconds("5 minutes"), None);
    assert_eq!(seconds("-1 second"), None);
    assert_eq!(seconds(""), None);
}

#[tokio::test]
async fn minimax_video_routes_by_model_and_strips_the_configured_base_url() {
    let sender = DynamicSender::new(|request, _| json_reply(request, json!({ "task_id": "t1" })));
    let (executor, context) = runtime(&sender);
    let settings = json!({ "apiKey": "test", "baseURL": "https://api.minimax.cn/v1/" });
    let h3 = video_model(MINIMAX_VIDEOS, "MiniMax-H3-Lite", settings.clone());
    let hailuo = video_model(MINIMAX_VIDEOS, "MiniMax-Hailuo-02", settings);

    VideoClient::start(video_request(&h3, "x"), &executor, &context).await.expect("started");
    VideoClient::start(video_request(&hailuo, "x"), &executor, &context).await.expect("started");

    assert_eq!(
        sender.recorded().iter().map(|call| call.url.as_str()).collect::<Vec<_>>(),
        vec!["https://api.minimax.cn/v2/video_generation", "https://api.minimax.cn/v1/video_generation"]
    );
}

#[tokio::test]
async fn video_protocols_reject_a_foreign_asset_as_invalid_request() {
    let openrouter = video_model(OPENROUTER_VIDEOS, "openrouter/auto", json!({ "apiKey": "test" }));
    let xai = video_model(XAI_VIDEOS, "grok-imagine-video-1.5", json!({ "apiKey": "test" }));
    let hailuo = video_model(MINIMAX_VIDEOS, "MiniMax-Hailuo-02", json!({ "apiKey": "test" }));
    let cases = [
        (
            "OpenRouter Video accepts inline bytes, data URLs, http(s) URLs; got google:files/x",
            openrouter.clone(),
            false,
        ),
        ("OpenRouter Video accepts inline bytes, data URLs, http(s) URLs; got google:files/x", openrouter, true),
        ("xAI Video accepts inline bytes, data URLs, http(s) URLs, and xai references; got google:files/x", xai, false),
        ("MiniMax Hailuo Video accepts inline bytes, data URLs, http(s) URLs; got google:files/x", hailuo, false),
    ];

    for (message, model, as_reference) in cases {
        let sender = DynamicSender::new(|_, _| panic!("a foreign asset reached the network"));
        let (executor, context) = runtime(&sender);
        let mut request = video_request(&model, "x");
        if as_reference {
            request.references = vec![provider_ref("google", "files/x")];
        } else {
            request.start = Some(provider_ref("google", "files/x"));
        }

        let error = VideoClient::start(request, &executor, &context).await.expect_err("foreign asset");
        assert_eq!(reason_tag(&error.reason), "InvalidRequest");
        assert_eq!(error.message(), message);
        assert!(sender.recorded().is_empty());
    }
}

fn running_snapshot() -> Snapshot {
    Snapshot { id: "job".to_string(), status: Status::Running, reason: None, progress: None }
}

fn queued_route<Request, Response>(id: &str, result: Response) -> QueuedRoute<Request, Response>
where
    Request: Send + 'static,
    Response: Clone + Send + Sync + 'static,
{
    let route = Arc::new(GenerationRoute::<Response> {
        status: Arc::new(|| Box::pin(async { Ok(running_snapshot()) })),
        result: Arc::new(move || {
            let result = result.clone();
            Box::pin(async move { Ok(result) })
        }),
        cancel: None,
    });
    QueuedRoute {
        kind: RouteKind::Queued,
        id: id.to_string(),
        provider: "test".to_string(),
        protocol: "test".to_string(),
        start: Arc::new({
            let route = route.clone();
            move |_, _, _| {
                let route = route.as_ref().clone();
                Box::pin(async move { Ok(Generation::new(route, json!("job"), running_snapshot())) })
            }
        }),
        resume: Arc::new({
            let route = route.clone();
            move |_, _, _, _| {
                let route = route.as_ref().clone();
                Box::pin(async move { Ok(Generation::new(route, json!("job"), running_snapshot())) })
            }
        }),
        attach: Arc::new(|_, _, _, _| Box::pin(async { Err(invalid_request()) })),
    }
}

fn empty_image_response() -> ImageResponse {
    ImageResponse { images: Vec::new(), usage: None, provider_metadata: None }
}

fn empty_speech_response() -> SpeechResponse {
    SpeechResponse { audio: data_url("audio/mpeg", "aGk="), timestamps: None, usage: None, provider_metadata: None }
}

fn queued_image_request() -> ImageRequest {
    let model = ImageModel {
        id: "queued-image".to_string(),
        provider: "test".into(),
        route: Arc::new(MediaRoute::Queued(queued_route::<ImageRequest, ImageResponse>(
            "queued-image",
            empty_image_response(),
        ))),
        transport: None,
        provider_options: None,
    };
    ImageRequest {
        model,
        prompt: "queued".to_string(),
        images: Vec::new(),
        n: None,
        aspect: None,
        resolution: None,
        provider_options: None,
        headers: None,
        timeout: None,
        http: None,
    }
}

fn queued_speech_model() -> SpeechModel {
    SpeechModel {
        id: "queued-speech".to_string(),
        provider: "test".into(),
        route: Arc::new(MediaRoute::Queued(queued_route::<SpeechRequest, SpeechResponse>(
            "queued-speech",
            empty_speech_response(),
        ))),
        transport: None,
        provider_options: None,
    }
}

fn queued_speech_request() -> SpeechRequest {
    SpeechRequestInput {
        text: "queued".to_string(),
        voice: None,
        format: None,
        speed: None,
        language: None,
        instructions: None,
        timestamps: None,
        provider_options: None,
        headers: None,
        timeout: None,
        http: None,
    }
    .with_model(queued_speech_model())
}

fn immediate_timeout() -> Option<AwaitOptions> {
    Some(AwaitOptions { poll: Some(Poll { interval: None, timeout: Some(Duration::ZERO) }) })
}

#[tokio::test]
async fn image_and_speech_generate_thread_await_options_to_queued_routes() {
    let sender = DynamicSender::new(|_, _| panic!("a queued generation reached the network"));
    let (executor, context) = runtime(&sender);

    let image = tokio::time::timeout(
        Duration::from_secs(2),
        ImageClient::generate(queued_image_request(), immediate_timeout(), &executor, &context),
    )
    .await
    .expect("image generate ignored its await options")
    .expect_err("the queued image status stays running");
    assert_eq!(reason_tag(&image.reason), "Timeout");

    let speech = tokio::time::timeout(
        Duration::from_secs(2),
        SpeechClient::generate(queued_speech_request(), immediate_timeout(), &executor, &context),
    )
    .await
    .expect("speech generate ignored its await options")
    .expect_err("the queued speech status stays running");
    assert_eq!(reason_tag(&speech.reason), "Timeout");
}

#[tokio::test]
async fn speech_start_and_resume_use_the_queued_route() {
    let sender = DynamicSender::new(|_, _| panic!("a queued speech start reached the network"));
    let (executor, context) = runtime(&sender);

    let started = SpeechClient::start(queued_speech_request(), &executor, &context).await.expect("started");
    assert_eq!(started.token, json!("job"));
    assert_eq!(started.status, Status::Running);

    let resumed =
        SpeechClient::resume(&queued_speech_request(), json!("job"), &executor, &context).await.expect("resumed");
    assert_eq!(resumed.token, json!("job"));
}

#[tokio::test]
async fn image_start_rejects_an_inline_route_as_unsupported() {
    let sender = DynamicSender::new(|_, _| panic!("an inline start reached the network"));
    let (executor, context) = runtime(&sender);
    let image_protocol = opencode_ai::protocols::minimax_images::protocol();
    let image_model = ImageModel::inline(
        "image-01",
        "minimax".into(),
        image_protocol,
        opencode_ai::media::route::MediaEndpoint::path("/image_generation"),
        opencode_ai::route::auth::Auth::headers(Vec::new()),
        None,
        None,
        None,
    );
    let image = ImageClient::start(
        ImageRequest {
            model: image_model,
            prompt: "inline".to_string(),
            images: Vec::new(),
            n: None,
            aspect: None,
            resolution: None,
            provider_options: None,
            headers: None,
            timeout: None,
            http: None,
        },
        &executor,
        &context,
    )
    .await
    .expect_err("an inline image route cannot start");
    match image.reason {
        AiErrorReason::UnsupportedOperation { operation, .. } => assert_eq!(operation, "image.start"),
        other => panic!("expected an unsupported operation, got {other:?}"),
    }
}

#[test]
fn realtime_connect_input_debug_redacts_the_api_key() {
    let input: RealtimeConnectInput = serde_json::from_value(json!({
        "engine": { "id": "xai-realtime", "provider": "xai", "api": "realtime", "transports": ["websocket"] },
        "model": "grok-realtime",
        "apiKey": "sk-secret-value",
        "transport": { "type": "websocket" }
    }))
    .expect("realtime connect input");
    let debug = format!("{input:?}");
    assert!(!debug.contains("sk-secret-value"), "{debug}");
    assert!(debug.contains("<redacted>"), "{debug}");
}

#[test]
fn openai_image_events_choose_partial_or_final_by_type() {
    let protocol = OpenAiImages::default_protocol();
    let completed = Frame::Event(
        json!({
            "type": "image_generation.completed",
            "b64_json": "aGk=",
            "output_format": "png",
            "partial_image_index": 3
        })
        .to_string(),
    );
    let (_, events) = protocol.step(State::default(), completed).expect("completed event");
    assert!(matches!(events.as_slice(), [ImageEvent::Image { index: 0, .. }]), "{events:?}");

    let partial = Frame::Event(
        json!({ "type": "image_generation.partial_image", "b64_json": "aGk=", "output_format": "png" }).to_string(),
    );
    let error = match protocol.step(State::default(), partial) {
        Ok(_) => panic!("a partial event without an index must fail"),
        Err(error) => error,
    };
    assert!(matches!(error.reason, AiErrorReason::InvalidProviderOutput { .. }));
}

#[tokio::test]
async fn minimax_images_rejects_a_resolution_without_a_tier_instead_of_using_a_preset() {
    let sender = DynamicSender::new(|_, _| panic!("a rejected resolution reached the network"));
    let (executor, context) = runtime(&sender);
    let settings = Settings::new(json!({ "apiKey": "test" }).as_object().expect("settings").clone());
    let model = match ProviderPackages::builtin().model(
        "@opencode/ai/providers/minimax/images",
        "image-01",
        &settings,
        &MapEnv::default(),
    ) {
        Ok(RuntimeModel::Image(model)) => model,
        Ok(_) => panic!("minimax images is not an image model"),
        Err(error) => panic!("{}", error.message()),
    };
    let request = ImageRequestInput {
        prompt: "a circle".to_string(),
        images: Vec::new(),
        n: None,
        aspect: Some("1:1".to_string()),
        resolution: Some("1024x1024".to_string()),
        provider_options: None,
        headers: None,
        timeout: None,
        http: None,
    }
    .with_model(model);

    let error = ImageClient::generate(request, None, &executor, &context)
        .await
        .expect_err("an unparseable resolution must not fall back to a preset");
    assert!(matches!(error.reason, AiErrorReason::UnsupportedOperation { .. }));
    assert!(sender.recorded().is_empty());
}
