use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use opencode_bus::{Bus, BusOptions, Event, ListenerHandle};
use opencode_catalog::models_dev::{CATALOG_PATH, DEFAULT_SOURCE, ModelsDevService, Options, body_digest, cache_key};
use opencode_storage::id::now_ms;
use opencode_storage::kv;
use opencode_storage::{Database, DatabaseLocation};
use serde_json::{Map, Value, json};
use tokio::task::JoinSet;

use crate::TempDir;

const FIXTURE: &str = r#"{"acme":{"id":"acme","name":"Acme","env":["ACME_API_KEY"],"models":{"acme-1":{"id":"acme-1","type":"chat","name":"Acme One","description":"A model","release_date":"2026-01-01","modalities":{"input":["text"],"output":["text"]},"capabilities":{"tools":{"supported":true},"reasoning":{"supported":false},"temperature":true},"limit":{"context":128000,"output":8192},"api":{"chat-completions":{"base_url":"https://api.acme.test/v1","sdk":{"aisdk":"@ai-sdk/openai-compatible","opencodeai":"@opencode/ai/providers/openai-compatible"},"compatibility":{"reasoning_field":"vendor_reasoning"}}}}}}}"#;
const FIXTURE2: &str = r#"{"beta":{"id":"beta","name":"Beta","env":["BETA_API_KEY"],"models":{"beta-1":{"id":"beta-1","type":"chat","name":"Beta One","description":"A model","release_date":"2026-02-01","modalities":{"input":["text"],"output":["text"]},"capabilities":{"tools":{"supported":false},"reasoning":{"supported":false},"temperature":false},"limit":{"context":64000,"output":4096},"api":{"chat-completions":{"base_url":"https://api.example.test/v1","sdk":{"aisdk":"@ai-sdk/openai-compatible","opencodeai":"@opencode/ai/providers/openai-compatible"}}}}}}}"#;
const V1_CATALOG: &str = r#"{"beta":{"id":"beta","name":"Beta","env":["BETA_API_KEY"],"npm":"@ai-sdk/openai-compatible","api":"https://api.beta.test/v1","models":{"beta-1":{"id":"beta-1","name":"Beta One","release_date":"2026-02-01","tool_call":true,"limit":{"context":64000,"output":4096}}}}}"#;
const KEY: &str = "models-dev:v3:catalog";
const USER_AGENT: &str = "opencode/test/1.0.0/test";

#[derive(Clone, Debug)]
struct Call {
    path: String,
    user_agent: Option<String>,
    if_none_match: Option<String>,
}

#[derive(Clone, Debug)]
struct Reply {
    status: u16,
    body: String,
    etag: Option<String>,
}

struct Stub {
    address: SocketAddr,
    calls: Arc<Mutex<Vec<Call>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Stub {
    fn start<F>(handler: F) -> Self
    where
        F: Fn(&Call) -> Reply + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a stub listener binds");
        let address = listener.local_addr().expect("a bound stub listener has an address");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let log = calls.clone();
        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if flag.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                serve(stream, &handler, &log);
            }
        });
        Self { address, calls, stop, thread: Some(thread) }
    }

    fn origin(&self) -> String {
        format!("http://{}", self.address)
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().expect("lock the stub calls").clone()
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve<F>(mut stream: TcpStream, handler: &F, calls: &Mutex<Vec<Call>>)
where
    F: Fn(&Call) -> Reply,
{
    let mut reader = BufReader::new(stream.try_clone().expect("a stub stream clones"));
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let mut parts = line.split_whitespace();
    let _method = parts.next();
    let path = parts.next().unwrap_or_default().to_owned();
    let mut headers = BTreeMap::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    let call = Call {
        path,
        user_agent: headers.get("user-agent").cloned(),
        if_none_match: headers.get("if-none-match").cloned(),
    };
    calls.lock().expect("lock the stub calls").push(call.clone());
    let reply = handler(&call);
    let etag = reply.etag.map(|etag| format!("ETag: {etag}\r\n")).unwrap_or_default();
    let response = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n{}",
        reply.status,
        reason(reply.status),
        reply.body.len(),
        etag,
        reply.body
    );
    let _ = stream.write_all(response.as_bytes());
}

fn reason(status: u16) -> &'static str {
    match status {
        304 => "Not Modified",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "OK",
    }
}

fn setup(
    stub: Option<&Stub>,
    fetch: bool,
    snapshot: bool,
) -> (TempDir, Arc<Database>, Arc<Bus>, Arc<ModelsDevService>) {
    let dir = TempDir::new();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    let database = Arc::new(database);
    let bus = Bus::new(database.clone(), BusOptions::default());
    let options = Options { url: stub.map(Stub::origin), file: None, fetch: Some(fetch), snapshot: Some(snapshot) };
    let service = ModelsDevService::new(database.clone(), bus.clone(), options, USER_AGENT.to_owned());
    (dir, database, bus, service)
}

fn put(database: &Database, key: &str, value: Value) {
    database.read(|connection| kv::set(connection, key, &value)).expect("write the kv entry");
}

fn get(database: &Database, key: &str) -> Option<Value> {
    database.read(|connection| kv::get(connection, key)).expect("read the kv entry")
}

fn cache_row(text: &str, updated_at: f64, digest: Option<&str>) -> Value {
    let mut row = Map::new();
    row.insert("updatedAt".to_owned(), json!(updated_at));
    row.insert("body".to_owned(), json!(text));
    if let Some(digest) = digest {
        row.insert("digest".to_owned(), json!(digest));
    }
    Value::Object(row)
}

fn capture(bus: &Bus) -> (Arc<Mutex<Vec<Event>>>, ListenerHandle) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let handle = bus.listen(Box::new(move |event: &Event| {
        sink.lock().expect("lock the events").push(event.clone());
        Ok(())
    }));
    (events, handle)
}

fn captured(events: &Arc<Mutex<Vec<Event>>>) -> Vec<(String, Value)> {
    events
        .lock()
        .expect("lock the events")
        .iter()
        .map(|event| (event.r#type().to_owned(), event.data().clone()))
        .collect()
}

fn provider_ids(snapshots: &[opencode_catalog::models_dev::Snapshot]) -> Vec<&str> {
    snapshots.iter().map(|snapshot| snapshot.info.id.0.as_str()).collect()
}

fn key_for(stub: &Stub) -> String {
    cache_key(&stub.origin())
}

#[tokio::test]
async fn get_returns_normalized_snapshots_from_cache() {
    let (_dir, database, _bus, service) = setup(None, false, true);
    put(&database, KEY, cache_row(FIXTURE, now_ms() as f64, Some(&body_digest(FIXTURE))));

    let snapshots = service.get(false).await.expect("the catalog loads");
    assert_eq!(provider_ids(&snapshots), ["acme"]);
    let model = &snapshots[0].models[0];
    assert_eq!(model.id, "acme-1");
    assert_eq!(model.name, "Acme One");
    assert!(model.capabilities.tools);
    assert_eq!(model.limit.context, 128000.0);
}

#[tokio::test]
async fn file_precedes_cache_and_snapshot() {
    let dir = TempDir::new();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    let database = Arc::new(database);
    let bus = Bus::new(database.clone(), BusOptions::default());
    let path = dir.path().join("catalog.json");
    std::fs::write(&path, FIXTURE2).expect("write the catalog file");
    put(&database, KEY, cache_row(FIXTURE, now_ms() as f64, Some(&body_digest(FIXTURE))));
    let options = Options { file: Some(path), fetch: Some(false), snapshot: Some(true), ..Default::default() };
    let service = ModelsDevService::new(database.clone(), bus.clone(), options, USER_AGENT.to_owned());

    let snapshots = service.get(false).await.expect("the catalog loads");
    assert_eq!(provider_ids(&snapshots), ["beta"]);
}

#[tokio::test]
async fn cache_precedes_bundled_snapshot() {
    let (_dir, database, _bus, service) = setup(None, false, true);
    put(&database, KEY, cache_row(FIXTURE, now_ms() as f64, Some(&body_digest(FIXTURE))));

    let snapshots = service.get(false).await.expect("the catalog loads");
    assert_eq!(provider_ids(&snapshots), ["acme"]);
}

#[tokio::test]
async fn bundled_snapshot_when_cache_is_empty() {
    let (_dir, _database, _bus, service) = setup(None, false, true);

    let snapshots = service.get(false).await.expect("the catalog loads");
    assert!(!snapshots.is_empty(), "the bundled snapshot holds providers");
    let anthropic = snapshots
        .iter()
        .find(|snapshot| snapshot.info.id.0 == "anthropic")
        .expect("the bundled snapshot holds anthropic");
    assert!(anthropic.environment.iter().any(|name| name == "ANTHROPIC_API_KEY"));
}

#[tokio::test]
async fn empty_when_fetch_and_snapshot_are_disabled() {
    let (_dir, _database, _bus, service) = setup(None, false, false);

    assert!(service.get(false).await.expect("the catalog loads").is_empty());
}

#[tokio::test]
async fn recovers_from_a_broken_cache_row() {
    let stub = Stub::start(|_| Reply { status: 200, body: FIXTURE2.to_owned(), etag: None });
    let (_dir, database, _bus, service) = setup(Some(&stub), true, false);
    put(&database, &key_for(&stub), json!({ "updatedAt": now_ms() as f64, "digest": "x", "body": "{" }));

    let snapshots = service.get(false).await.expect("the catalog loads");
    assert_eq!(provider_ids(&snapshots), ["beta"]);
    assert_eq!(stub.calls().len(), 1);
    let row = get(&database, &key_for(&stub)).expect("the cache row is rewritten");
    assert_eq!(row["body"].as_str(), Some(FIXTURE2));
    assert_eq!(row["digest"].as_str(), Some(body_digest(FIXTURE2).as_str()));
}

#[tokio::test]
async fn custom_mirror_falls_back_to_api_json() {
    let stub = Stub::start(|call| {
        if call.path.starts_with("/experimental/") {
            return Reply { status: 404, body: "{}".to_owned(), etag: None };
        }
        Reply { status: 200, body: V1_CATALOG.to_owned(), etag: None }
    });
    let (_dir, _database, _bus, service) = setup(Some(&stub), true, false);

    let snapshots = service.get(false).await.expect("the catalog loads");
    assert_eq!(provider_ids(&snapshots), ["beta"]);
    let paths = stub.calls().iter().map(|call| call.path.clone()).collect::<Vec<_>>();
    assert_eq!(paths, [CATALOG_PATH.to_owned(), "/api.json".to_owned()]);
}

#[tokio::test]
async fn refresh_fetches_and_updates_the_cache() {
    let stub = Stub::start(|_| Reply { status: 200, body: FIXTURE2.to_owned(), etag: Some("W/\"v2\"".to_owned()) });
    let (_dir, database, _bus, service) = setup(Some(&stub), false, false);
    let key = key_for(&stub);
    put(&database, &key, cache_row(FIXTURE, now_ms() as f64, Some(&body_digest(FIXTURE))));
    assert_eq!(provider_ids(&service.get(false).await.expect("the cached catalog loads")), ["acme"]);

    service.refresh(true).await;
    assert_eq!(provider_ids(&service.get(false).await.expect("the refreshed catalog loads")), ["beta"]);

    let digest = body_digest(FIXTURE2);
    let row = get(&database, &key).expect("the cache row is updated");
    assert_eq!(row["body"].as_str(), Some(FIXTURE2));
    assert_eq!(row["digest"].as_str(), Some(digest.as_str()));
    assert!(row["updatedAt"].as_f64().is_some());
    let meta = get(&database, &format!("{key}:meta")).expect("the meta row is updated");
    assert_eq!(meta["etag"].as_str(), Some("W/\"v2\""));
    assert_eq!(meta["digest"].as_str(), Some(digest.as_str()));

    let calls = stub.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].path, CATALOG_PATH);
    assert_eq!(calls[0].user_agent.as_deref(), Some(USER_AGENT));
    assert_eq!(calls[0].if_none_match, None);
}

#[tokio::test]
async fn refresh_skips_a_fresh_cache() {
    let stub = Stub::start(|_| Reply { status: 200, body: FIXTURE2.to_owned(), etag: None });
    let (_dir, database, _bus, service) = setup(Some(&stub), false, false);
    put(&database, &key_for(&stub), cache_row(FIXTURE, now_ms() as f64 - 1000.0, Some(&body_digest(FIXTURE))));

    service.refresh(false).await;
    assert!(stub.calls().is_empty());
}

#[tokio::test]
async fn refresh_fetches_stale_cache_and_publishes_globally() {
    let stub = Stub::start(|_| Reply { status: 200, body: FIXTURE2.to_owned(), etag: None });
    let (_dir, database, bus, service) = setup(Some(&stub), false, false);
    put(&database, &key_for(&stub), cache_row(FIXTURE, now_ms() as f64 - 600000.0, Some(&body_digest(FIXTURE))));
    let (events, _handle) = capture(&bus);

    service.refresh(false).await;
    assert_eq!(captured(&events), [("models-dev.refreshed".to_owned(), json!({}))]);
    assert!(events.lock().expect("lock the events")[0].location().is_none());
    assert_eq!(stub.calls().len(), 1);
    assert_eq!(provider_ids(&service.get(false).await.expect("the refreshed catalog loads")), ["beta"]);
}

#[tokio::test]
async fn refresh_stays_quiet_when_digest_matches() {
    let stub = Stub::start(|_| Reply { status: 200, body: FIXTURE.to_owned(), etag: None });
    let (_dir, database, bus, service) = setup(Some(&stub), false, false);
    let digest = body_digest(FIXTURE);
    let key = key_for(&stub);
    put(&database, &key, cache_row(FIXTURE, now_ms() as f64 - 600000.0, Some(&digest)));
    let body = get(&database, &key).expect("the cache row exists");
    let (events, _handle) = capture(&bus);

    service.refresh(false).await;
    assert!(captured(&events).is_empty());
    assert_eq!(get(&database, &key), Some(body));
    let meta = get(&database, &format!("{key}:meta")).expect("the meta row is recorded");
    assert_eq!(meta["digest"].as_str(), Some(digest.as_str()));
    assert!(now_ms() as f64 - meta["updatedAt"].as_f64().expect("an updatedAt") < 60_000.0);
    assert_eq!(stub.calls().len(), 1);
}

#[tokio::test]
async fn refresh_revalidates_with_etag_and_treats_304_as_unchanged() {
    let reply =
        Arc::new(Mutex::new(Reply { status: 200, body: FIXTURE.to_owned(), etag: Some("W/\"v1\"".to_owned()) }));
    let stub = Stub::start({
        let reply = reply.clone();
        move |_| reply.lock().expect("lock the stub reply").clone()
    });
    let (_dir, database, bus, service) = setup(Some(&stub), false, false);

    service.refresh(true).await;
    let meta_key = format!("{}:meta", key_for(&stub));
    let mut meta = get(&database, &meta_key).expect("the meta row exists").as_object().expect("an object").clone();
    meta.insert("updatedAt".to_owned(), json!(now_ms() as f64 - 600000.0));
    put(&database, &meta_key, Value::Object(meta));

    *reply.lock().expect("lock the stub reply") = Reply { status: 304, body: String::new(), etag: None };
    let (events, _handle) = capture(&bus);
    service.refresh(false).await;
    assert!(captured(&events).is_empty());
    assert_eq!(provider_ids(&service.get(false).await.expect("the cached catalog loads")), ["acme"]);
    let meta = get(&database, &meta_key).expect("the meta row is revalidated");
    assert_eq!(meta["etag"].as_str(), Some("W/\"v1\""));
    assert!(now_ms() as f64 - meta["updatedAt"].as_f64().expect("an updatedAt") < 60_000.0);

    *reply.lock().expect("lock the stub reply") = Reply { status: 304, body: String::new(), etag: None };
    let (forced, _forced_handle) = capture(&bus);
    service.refresh(true).await;
    assert!(captured(&forced).is_empty());

    let etags = stub.calls().into_iter().map(|call| call.if_none_match).collect::<Vec<_>>();
    assert_eq!(etags, [None, Some("W/\"v1\"".to_owned()), None]);
}

#[tokio::test]
async fn removes_legacy_cache_keys() {
    let dir = TempDir::new();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    let database = Arc::new(database);
    let bus = Bus::new(database.clone(), BusOptions::default());
    let legacy =
        ["models-dev:catalog", "models-dev:catalog:meta", "models-dev:v2:catalog", "models-dev:v2:catalog:meta"];
    legacy.iter().for_each(|key| put(&database, key, cache_row(FIXTURE, now_ms() as f64, Some(&body_digest(FIXTURE)))));
    let options = Options { fetch: Some(false), snapshot: Some(true), ..Default::default() };
    let service = ModelsDevService::new(database.clone(), bus.clone(), options, USER_AGENT.to_owned());

    legacy.iter().for_each(|key| assert_eq!(get(&database, key), None, "removes {key}"));
    assert!(!service.get(false).await.expect("the catalog loads").is_empty());
}

#[tokio::test]
async fn legacy_entry_without_digest_republishes_once() {
    let stub = Stub::start(|_| Reply { status: 200, body: FIXTURE.to_owned(), etag: None });
    let (_dir, database, bus, service) = setup(Some(&stub), false, false);
    put(&database, &key_for(&stub), cache_row(FIXTURE, now_ms() as f64 - 600000.0, None));
    let (events, _handle) = capture(&bus);

    service.refresh(false).await;
    assert_eq!(captured(&events).len(), 1);
    let row = get(&database, &key_for(&stub)).expect("the cache row exists");
    assert_eq!(row["digest"].as_str(), Some(body_digest(FIXTURE).as_str()));
}

#[tokio::test]
async fn refresh_swallows_http_errors() {
    let stub = Stub::start(|_| Reply { status: 500, body: "boom".to_owned(), etag: None });
    let (_dir, database, _bus, service) = setup(Some(&stub), false, false);
    put(&database, &key_for(&stub), cache_row(FIXTURE, now_ms() as f64, Some(&body_digest(FIXTURE))));

    service.refresh(true).await;
    assert_eq!(provider_ids(&service.get(false).await.expect("the cached catalog loads")), ["acme"]);
    assert!(!stub.calls().is_empty());
}

#[tokio::test]
async fn custom_source_cache_key_uses_sha1() {
    let stub = Stub::start(|_| Reply { status: 200, body: FIXTURE.to_owned(), etag: None });
    let (_dir, database, _bus, service) = setup(Some(&stub), false, false);

    service.refresh(true).await;
    let key = cache_key(&stub.origin());
    let row = get(&database, &key).expect("the custom cache row exists");
    assert_eq!(row["body"].as_str(), Some(FIXTURE));
    assert!(get(&database, &format!("{key}:meta")).is_some());
    assert_eq!(cache_key(DEFAULT_SOURCE), KEY);
    assert_eq!(cache_key("https://mirror.example"), "models-dev:v3:catalog:5fad74904de66c0ef95d32100c082f56dfa1f60b");
}

#[tokio::test]
async fn get_is_single_flight() {
    let stub = Stub::start(|_| Reply { status: 200, body: FIXTURE.to_owned(), etag: None });
    let (_dir, _database, _bus, service) = setup(Some(&stub), true, false);
    let mut tasks = JoinSet::new();
    (0..5).for_each(|_| {
        let service = service.clone();
        tasks.spawn(async move { service.get(false).await.expect("the catalog loads") });
    });

    let mut results = Vec::new();
    while let Some(result) = tasks.join_next().await {
        results.push(result.expect("the task finishes"));
    }
    results.iter().for_each(|snapshots| assert_eq!(provider_ids(snapshots), ["acme"]));
    assert_eq!(stub.calls().len(), 1);
}

#[tokio::test]
async fn options_from_vars_reads_the_environment() {
    let vars = HashMap::from([
        ("OPENCODE_MODELS_URL", OsString::from("https://mirror.example")),
        ("OPENCODE_MODELS_PATH", OsString::from("/tmp/catalog.json")),
        ("OPENCODE_DISABLE_MODELS_FETCH", OsString::from("true")),
    ]);
    let options = Options::from_vars(|name| vars.get(name).cloned());

    assert_eq!(options.url.as_deref(), Some("https://mirror.example"));
    assert_eq!(options.file, Some(PathBuf::from("/tmp/catalog.json")));
    assert_eq!(options.fetch, Some(false));
    assert_eq!(options.snapshot, None);

    let empty = Options::from_vars(|_| None);
    assert_eq!(empty.url, None);
    assert_eq!(empty.file, None);
    assert_eq!(empty.fetch, Some(true));
    assert_eq!(empty.snapshot, None);
}
