use std::future::Future;
use std::path::Path;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{Value, json};

use opencode_sidecar::connection::{Handlers, Request};
use opencode_sidecar::supervisor::Sidecar;

use crate::support;

const TIMEOUT: Duration = Duration::from_secs(60);
const SMALL_WARMUP: usize = 200;
const SMALL_SAMPLES: usize = 2_000;
const BINARY_WARMUP: usize = 20;
const BINARY_SAMPLES: usize = 200;
const BINARY_BYTES: usize = 1 << 20;

async fn within<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(TIMEOUT, future).await.expect("in time")
}

#[tokio::test]
#[ignore = "measures a real Bun sidecar: run with --ignored --nocapture"]
async fn measure() {
    let mut config = support::config();
    config.limits.max_frame = 4 << 20;
    let Some(runtime) = support::runtime(&config).await else { return };

    let sidecar = Sidecar::new(config, Handlers::new());
    let warm = json!({ "warm": true });
    let reply = within(sidecar.call(Request::new("host.echo", warm.clone()))).await.expect("a warm-up");
    assert_eq!(reply.value, warm);

    tokio::time::sleep(Duration::from_millis(300)).await;
    let pid = sidecar.pid().expect("a running host");
    let idle = Memory::read(pid);

    load(&sidecar, "effect.ts", json!(["value"])).await;
    let effect = Memory::read(pid);
    load(&sidecar, "sample-plugin.ts", json!(["default"])).await;
    let plugin = Memory::read(pid);
    let reply = within(sidecar.call(Request::new("plugin.resolve", json!({ "directory": fixture_directory() }))))
        .await
        .expect("the first plugin method");
    assert!(reply.value.is_object());
    let first_plugin_method = Memory::read(pid);

    let small = small_latency(&sidecar).await;
    let binary = binary_latency(&sidecar).await;

    sidecar.shutdown().await;

    let bun = match &runtime.version {
        Some(version) => version.clone(),
        None => runtime.bun.file_name().expect("a runtime file name").to_string_lossy().into_owned(),
    };
    let cpus = std::thread::available_parallelism().map(|cpus| cpus.get()).unwrap_or(1);
    let platform = format!("{} {}, {cpus} CPUs", std::env::consts::OS, std::env::consts::ARCH);
    let plugins = std::fs::metadata(Path::new(env!("CARGO_MANIFEST_DIR")).join("host/plugins.js"))
        .expect("the plugin bundle")
        .len() as f64
        / (1024.0 * 1024.0);

    println!(
        "| Measurement | Value |\n\
         | --- | --- |\n\
         | Bun | {bun} |\n\
         | Platform | {platform} |\n\
         | plugins.js | {plugins:.1} MiB |\n\
         | Memory, idle: RSS / peak | {idle} |\n\
         | Memory, Effect loaded: RSS / peak | {effect} |\n\
         | Memory, sample plugin loaded: RSS / peak | {plugin} |\n\
         | Memory, first plugin method: RSS / peak | {first_plugin_method} |\n\
         | Latency, small echo: p50 / p99 | {small} |\n\
         | Latency, 1 MiB echo: p50 / p99 | {binary} |"
    );
}

async fn load(sidecar: &Sidecar, name: &str, exports: Value) {
    let reply =
        within(sidecar.call(Request::new("host.import", json!({ "path": fixture(name) })))).await.expect("an import");
    assert_eq!(reply.value, exports, "{name}");
}

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../host/test/fixtures").join(name);
    std::fs::canonicalize(&path).expect("a fixture").to_str().expect("a UTF-8 path").to_owned()
}

fn fixture_directory() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../host/test/fixtures");
    std::fs::canonicalize(&path).expect("the fixture directory").to_str().expect("a UTF-8 path").to_owned()
}

async fn small_latency(sidecar: &Sidecar) -> Stats {
    let params = json!({ "n": 1 });
    for _ in 0..SMALL_WARMUP {
        let reply = within(sidecar.call(Request::new("host.echo", params.clone()))).await.expect("a warm-up");
        assert_eq!(reply.value, params);
    }
    let mut samples = Vec::with_capacity(SMALL_SAMPLES);
    for _ in 0..SMALL_SAMPLES {
        let started = Instant::now();
        let reply = within(sidecar.call(Request::new("host.echo", params.clone()))).await.expect("a small echo");
        samples.push(started.elapsed());
        assert_eq!(reply.value, params);
    }
    Stats::new(samples)
}

async fn binary_latency(sidecar: &Sidecar) -> Stats {
    let params = json!({ "kind": "binary" });
    let payload = Bytes::from(vec![7u8; BINARY_BYTES]);
    for _ in 0..BINARY_WARMUP {
        let request = Request::new("host.echo", params.clone()).with_bytes(payload.clone());
        let reply = within(sidecar.call(request)).await.expect("a warm-up");
        assert_eq!(reply.value, params);
        assert_eq!(reply.bytes.as_ref(), Some(&payload));
    }
    let mut samples = Vec::with_capacity(BINARY_SAMPLES);
    for _ in 0..BINARY_SAMPLES {
        let request = Request::new("host.echo", params.clone()).with_bytes(payload.clone());
        let started = Instant::now();
        let reply = within(sidecar.call(request)).await.expect("a 1 MiB echo");
        samples.push(started.elapsed());
        assert_eq!(reply.value, params);
        assert_eq!(reply.bytes.as_ref(), Some(&payload));
    }
    Stats::new(samples)
}

struct Memory {
    rss: f64,
    peak: f64,
}

impl Memory {
    fn read(pid: u32) -> Memory {
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).expect("a process status");
        Memory { rss: mib(&status, "VmRSS:"), peak: mib(&status, "VmHWM:") }
    }
}

impl std::fmt::Display for Memory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:.1} MiB / {:.1} MiB", self.rss, self.peak)
    }
}

fn mib(status: &str, field: &str) -> f64 {
    let line = status.lines().find(|line| line.starts_with(field)).expect("a memory field");
    let kib = line.split_whitespace().nth(1).expect("a value").parse::<f64>().expect("a number");
    kib / 1024.0
}

struct Stats {
    median: f64,
    p99: f64,
}

impl Stats {
    fn new(samples: Vec<Duration>) -> Stats {
        let mut sorted = samples.iter().map(|sample| sample.as_secs_f64() * 1_000.0).collect::<Vec<_>>();
        sorted.sort_by(f64::total_cmp);
        Stats { median: percentile(&sorted, 0.5), p99: percentile(&sorted, 0.99) }
    }
}

impl std::fmt::Display for Stats {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:.2} ms / {:.2} ms", self.median, self.p99)
    }
}

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    let rank = (fraction * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}
