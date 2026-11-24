use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use opencode_config::seams::{BoxFuture, KvStore, WellKnownEntry};
use opencode_config::wellknown::WellKnown;
use serde_json::{Value, json};
use tokio::sync::broadcast;

struct Request {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
}

struct Stub {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Stub {
    fn start<F>(handler: F) -> Self
    where
        F: Fn(&Request, &str) -> (u16, String) + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a stub listener binds");
        let address = listener.local_addr().expect("a bound stub listener has an address");
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || {
            let origin = format!("http://{address}");
            for stream in listener.incoming() {
                if flag.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else {
                    continue;
                };
                serve(stream, &handler, &origin);
            }
        });
        Self { address, stop, thread: Some(thread) }
    }

    fn origin(&self) -> String {
        format!("http://{}", self.address)
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

fn serve<F>(mut stream: TcpStream, handler: &F, origin: &str)
where
    F: Fn(&Request, &str) -> (u16, String),
{
    let mut reader = BufReader::new(stream.try_clone().expect("a stub stream clones"));
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
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
    let (status, body) = handler(&Request { method, path, headers }, origin);
    let response = format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

struct MemoryKv {
    values: Mutex<BTreeMap<String, Value>>,
}

impl MemoryKv {
    fn new() -> Self {
        Self { values: Mutex::new(BTreeMap::new()) }
    }

    fn read(&self, key: &str) -> Option<Value> {
        self.values.lock().expect("the memory kv lock is not poisoned").get(key).cloned()
    }
}

impl KvStore for MemoryKv {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Option<Value>> {
        Box::pin(async move { self.values.lock().expect("the memory kv lock is not poisoned").get(key).cloned() })
    }

    fn set<'a>(&'a self, key: &'a str, value: Value) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.values.lock().expect("the memory kv lock is not poisoned").insert(key.to_owned(), value);
        })
    }

    fn remove<'a>(&'a self, key: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.values.lock().expect("the memory kv lock is not poisoned").remove(key);
        })
    }
}

fn wellknown(kv: &Arc<MemoryKv>) -> WellKnown {
    WellKnown::new(kv.clone())
}

fn auth_manifest() -> Value {
    json!({ "auth": { "command": ["login"], "env": "TOKEN" } })
}

#[tokio::test]
async fn inspect_and_resolve_embedded_and_remote_configuration() {
    let stub = Stub::start(|request, origin| {
        if request.method != "GET" {
            return (405, "{}".to_owned());
        }
        match request.path.as_str() {
            "/.well-known/opencode" => (
                200,
                json!({
                    "auth": { "command": ["login"], "env": "TOKEN" },
                    "config": { "model": "embedded/model" },
                    "remote_config": {
                        "url": format!("{origin}/config/{{env:TOKEN}}"),
                        "headers": { "authorization": "Bearer {env:TOKEN}" }
                    }
                })
                .to_string(),
            ),
            "/config/secret" => {
                if request.headers.get("authorization").map(String::as_str) != Some("Bearer secret") {
                    return (401, "{}".to_owned());
                }
                (200, json!({ "config": { "model": "remote/model" } }).to_string())
            }
            _ => (404, "Not found".to_owned()),
        }
    });

    let kv = Arc::new(MemoryKv::new());
    let client = wellknown(&kv);
    let origin = stub.origin();
    let inspected = client.inspect(&format!("{origin}/")).await.expect("the manifest loads");
    assert_eq!(
        inspected,
        json!({
            "auth": { "command": ["login"], "env": "TOKEN" },
            "config": { "model": "embedded/model" },
            "remote_config": {
                "url": format!("{origin}/config/{{env:TOKEN}}"),
                "headers": { "authorization": "Bearer {env:TOKEN}" }
            }
        })
    );

    let entry = WellKnownEntry { origin: origin.clone(), integration_id: origin.clone(), manifest: inspected };
    let variables = BTreeMap::from([("TOKEN".to_owned(), "secret".to_owned())]);
    let configs = client.resolve(&entry, &variables).await.expect("the remote config loads");
    assert_eq!(configs, vec![json!({ "model": "embedded/model" }), json!({ "model": "remote/model" })]);
    assert_eq!(
        kv.read(&format!("wellknown:config:{origin}")),
        Some(json!([{ "model": "embedded/model" }, { "model": "remote/model" }]))
    );
}

#[tokio::test]
async fn add_persists_sources_and_skips_unreachable_ones() {
    let stub = Stub::start(|_, _| (200, auth_manifest().to_string()));
    let kv = Arc::new(MemoryKv::new());
    let client = wellknown(&kv);
    let origin = stub.origin();

    let mut updates = client.subscribe();
    let entry = client.add(&format!("{origin}/")).await.expect("the source adds");
    assert_eq!(entry.origin, origin);
    assert_eq!(kv.read("wellknown:sources"), Some(json!([origin.clone()])));
    assert_eq!(client.entries().await, vec![entry.clone()]);
    assert!(matches!(updates.try_recv(), Ok(())));
    assert!(matches!(updates.try_recv(), Err(broadcast::error::TryRecvError::Empty)));

    client.remove(&origin).await;
    assert_eq!(kv.read("wellknown:sources"), Some(json!([])));
    assert!(client.entries().await.is_empty());

    kv.set("wellknown:sources", json!(["http://127.0.0.1:1", origin.clone()])).await;
    let entries = client.entries().await;
    assert_eq!(entries.iter().map(|entry| entry.origin.as_str()).collect::<Vec<_>>(), vec![origin.as_str()]);
}

#[tokio::test]
async fn refresh_reports_changed_manifests() {
    let command = Arc::new(Mutex::new("first".to_owned()));
    let stub = Stub::start({
        let command = command.clone();
        move |_, _| {
            let command = command.lock().expect("the command lock is not poisoned").clone();
            (200, json!({ "auth": { "command": [command], "env": "TOKEN" } }).to_string())
        }
    });
    let kv = Arc::new(MemoryKv::new());
    let client = wellknown(&kv);
    let origin = stub.origin();
    client.add(&origin).await.expect("the source adds");
    assert!(!client.refresh().await);

    let mut updates = client.subscribe();
    *command.lock().expect("the command lock is not poisoned") = "second".to_owned();
    assert!(client.refresh().await);
    assert!(matches!(updates.try_recv(), Ok(())));
    assert!(matches!(updates.try_recv(), Err(broadcast::error::TryRecvError::Empty)));
    assert_eq!(client.snapshot()[0].manifest["auth"]["command"], json!(["second"]));

    assert!(!WellKnown::new(Arc::new(MemoryKv::new())).refresh().await);
}

#[tokio::test]
async fn entries_keep_the_cached_manifest_when_the_origin_is_down() {
    let stub = Stub::start(|_, _| (200, auth_manifest().to_string()));
    let kv = Arc::new(MemoryKv::new());
    let origin = stub.origin();
    wellknown(&kv).add(&origin).await.expect("the source adds");
    drop(stub);

    let entries = wellknown(&kv).entries().await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].origin, origin);
    assert_eq!(entries[0].manifest, auth_manifest());
}

#[tokio::test]
async fn remove_clears_the_sources_manifest_and_config() {
    let stub = Stub::start(|request, origin| match request.path.as_str() {
        "/.well-known/opencode" => (
            200,
            json!({
                "auth": { "command": ["login"], "env": "TOKEN" },
                "remote_config": { "url": format!("{origin}/config") }
            })
            .to_string(),
        ),
        "/config" => (200, json!({ "model": "remote/model" }).to_string()),
        _ => (404, "{}".to_owned()),
    });
    let kv = Arc::new(MemoryKv::new());
    let client = wellknown(&kv);
    let origin = stub.origin();
    let entry = client.add(&origin).await.expect("the source adds");
    let configs = client.resolve(&entry, &BTreeMap::new()).await.expect("the remote config loads");
    assert_eq!(configs, vec![json!({ "model": "remote/model" })]);
    assert!(kv.read(&format!("wellknown:manifest:{origin}")).is_some());
    assert!(kv.read(&format!("wellknown:config:{origin}")).is_some());

    client.remove(&origin).await;
    assert_eq!(kv.read("wellknown:sources"), Some(json!([])));
    assert_eq!(kv.read(&format!("wellknown:manifest:{origin}")), None);
    assert_eq!(kv.read(&format!("wellknown:config:{origin}")), None);
}

#[tokio::test]
async fn add_requires_an_authentication_method() {
    let stub = Stub::start(|_, _| (200, json!({ "config": {} }).to_string()));
    let error = wellknown(&Arc::new(MemoryKv::new())).add(&stub.origin()).await.expect_err("the source has no auth");
    assert!(error.to_string().contains("No authentication method found at"));
}

#[tokio::test]
async fn inspect_rejects_bad_responses_and_manifests() {
    let failing = Stub::start(|_, _| (500, "{}".to_owned()));
    let error = wellknown(&Arc::new(MemoryKv::new())).inspect(&failing.origin()).await.expect_err("a 500 fails");
    assert!(error.to_string().contains("Failed to load wellknown manifest from"));

    let invalid = Stub::start(|_, _| (200, json!({ "auth": { "command": ["login"] } }).to_string()));
    let error =
        wellknown(&Arc::new(MemoryKv::new())).inspect(&invalid.origin()).await.expect_err("the manifest is invalid");
    assert!(error.to_string().contains("Failed to load wellknown manifest from"));
}

#[tokio::test]
async fn resolve_rejects_invalid_and_missing_remote_configurations() {
    let invalid = Stub::start(|request, origin| match request.path.as_str() {
        "/.well-known/opencode" => (200, json!({ "remote_config": { "url": format!("{origin}/remote") } }).to_string()),
        "/remote" => (200, "[1,2]".to_owned()),
        _ => (404, "{}".to_owned()),
    });
    let client = wellknown(&Arc::new(MemoryKv::new()));
    let origin = invalid.origin();
    let manifest = client.inspect(&origin).await.expect("the manifest loads");
    let entry = WellKnownEntry { origin: origin.clone(), integration_id: origin.clone(), manifest };
    let error = client.resolve(&entry, &BTreeMap::new()).await.expect_err("a non-object remote config fails");
    assert!(error.to_string().contains("Failed to load wellknown remote config from"));

    let missing = Stub::start(|request, origin| match request.path.as_str() {
        "/.well-known/opencode" => {
            (200, json!({ "remote_config": { "url": format!("{origin}/missing") } }).to_string())
        }
        _ => (404, "not found".to_owned()),
    });
    let origin = missing.origin();
    let manifest = client.inspect(&origin).await.expect("the manifest loads");
    let entry = WellKnownEntry { origin: origin.clone(), integration_id: origin.clone(), manifest };
    let error = client.resolve(&entry, &BTreeMap::new()).await.expect_err("a missing remote config fails");
    assert!(error.to_string().contains("Failed to load wellknown remote config from"));
}
