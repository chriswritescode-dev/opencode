use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::prelude::{BASE64_STANDARD, Engine};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};

use opencode_ai::ExecutionContext;
use opencode_ai::MapEnv;
use opencode_ai::Sleeper;
use opencode_ai::clock::Clock;
use opencode_ai::id::SequenceIds;
use opencode_ai::media::generation::{Generation, GenerationRoute, Snapshot, Status};
use opencode_ai::media::multipart::{AppendFieldsOptions, Form, RepeatArrays, append_fields};
use opencode_ai::media::route::read_with_retry;
use opencode_ai::schema::{AiError, AiErrorReason};

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
    fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()> {
        self.durations.lock().unwrap().push(duration);
        self.clock.now.fetch_add(duration.as_millis() as u64, Ordering::SeqCst);
        Box::pin(async {})
    }
}

type Attempt<T> = Arc<dyn Fn() -> BoxFuture<'static, Result<T, AiError>> + Send + Sync>;

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
