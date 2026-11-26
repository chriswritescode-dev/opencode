use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use opencode_ai::route::executor::{BoxFuture, HttpResponse, HttpSender, PreparedHttp};
use opencode_ai::schema::AiError;
use opencode_catalog::builtin::Builtin;
use opencode_catalog::provider_plugin::opencode;
use opencode_catalog::provider_plugin::opencode::artifacts::HEADER;
use opencode_catalog::provider_plugin::opencode::watches;
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    ArtifactCapability, ArtifactProvider, CredentialKey, CredentialKeyType, CredentialValue,
};
use serde_json::{Value, json};

use crate::builtin::{Harness, Input, start};
use crate::provider_plugin::support::{PluginState, plugin_context_state};

const ORIGIN: &str = "http://127.0.0.1:1";

fn credential() -> CredentialValue {
    CredentialValue::Key(CredentialKey {
        r#type: CredentialKeyType::Key,
        key: "secret".to_owned(),
        metadata: Some(JsonMap(vec![("server".to_owned(), json!(ORIGIN))])),
    })
}

fn files() -> BTreeMap<String, Bytes> {
    BTreeMap::from([
        ("app.js".to_owned(), Bytes::from("console.log(1)")),
        ("index.html".to_owned(), Bytes::from("<h1>Hi</h1>")),
    ])
}

#[derive(Default)]
struct Console {
    artifacts: Mutex<bool>,
    watches: Mutex<bool>,
    blobs: Mutex<BTreeMap<String, Bytes>>,
    blob_reads: Mutex<Vec<String>>,
    uploads: Mutex<Vec<String>>,
    prepared: Mutex<Vec<Value>>,
    published: Mutex<Vec<(String, Value)>>,
    deleted: Mutex<Vec<String>>,
    gone: Mutex<bool>,
}

impl Console {
    fn set_artifacts(&self, value: bool) {
        *self.artifacts.lock().expect("the artifact flag lock is never poisoned") = value;
    }

    fn set_watches(&self, value: bool) {
        *self.watches.lock().expect("the watch flag lock is never poisoned") = value;
    }

    fn set_gone(&self, value: bool) {
        *self.gone.lock().expect("the gone flag lock is never poisoned") = value;
    }

    fn prepared(&self) -> Vec<Value> {
        self.prepared.lock().expect("the prepared lock is never poisoned").clone()
    }

    fn published(&self) -> Vec<(String, Value)> {
        self.published.lock().expect("the published lock is never poisoned").clone()
    }

    fn uploads(&self) -> Vec<String> {
        self.uploads.lock().expect("the uploads lock is never poisoned").clone()
    }

    fn blob_reads(&self) -> Vec<String> {
        self.blob_reads.lock().expect("the blob reads lock is never poisoned").clone()
    }

    fn deleted(&self) -> Vec<String> {
        self.deleted.lock().expect("the deleted lock is never poisoned").clone()
    }

    fn respond(&self, request: &PreparedHttp) -> Reply {
        let Ok(url) = url::Url::parse(&request.url) else {
            return not_found();
        };
        let path = url.path();
        let method = request.method.to_uppercase();
        match (method.as_str(), path) {
            ("GET", "/api/v2/config") => {
                let mut headers = Vec::new();
                if *self.artifacts.lock().expect("the artifact flag lock is never poisoned") {
                    headers.push((HEADER.to_owned(), "1".to_owned()));
                }
                if *self.watches.lock().expect("the watch flag lock is never poisoned") {
                    headers.push((watches::HEADER.to_owned(), "1".to_owned()));
                }
                (200, headers, Bytes::from_static(b"{}"))
            }
            ("POST", "/api/artifacts/prepare") => {
                let body: Value = serde_json::from_slice(&request.body).expect("the prepare request body is JSON");
                let missing = {
                    let blobs = self.blobs.lock().expect("the blobs lock is never poisoned");
                    body["files"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|file| file["sha256"].as_str())
                        .filter(|sha256| !blobs.contains_key(*sha256))
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                };
                self.prepared.lock().expect("the prepared lock is never poisoned").push(body);
                (200, json_headers(), json_body(json!({ "missing": missing })))
            }
            _ if method == "PUT" && path.starts_with("/api/artifacts/blobs/") => {
                let sha256 = last_segment(path).to_owned();
                self.uploads.lock().expect("the uploads lock is never poisoned").push(sha256.clone());
                self.blobs
                    .lock()
                    .expect("the blobs lock is never poisoned")
                    .insert(sha256.clone(), request.body.clone());
                (200, json_headers(), json_body(json!({ "sha256": sha256, "bytes": request.body.len() })))
            }
            _ if method == "GET" && path.ends_with("/files") => {
                if *self.gone.lock().expect("the gone flag lock is never poisoned") {
                    return not_found();
                }
                let published = self.published.lock().expect("the published lock is never poisoned");
                let revision = published.len();
                let listed = published
                    .last()
                    .map(|(_, body)| body["files"].as_array().cloned().unwrap_or_default())
                    .unwrap_or_default();
                let blobs = self.blobs.lock().expect("the blobs lock is never poisoned");
                let files = listed
                    .iter()
                    .map(|file| {
                        let path = file["path"].as_str().unwrap_or_default();
                        let sha256 = file["sha256"].as_str().unwrap_or_default();
                        let mut listed = json!({ "path": path, "sha256": sha256 });
                        if path == "index.html" {
                            let text = blobs
                                .get(sha256)
                                .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                                .unwrap_or_default();
                            listed["text"] = json!(text);
                        }
                        listed
                    })
                    .collect::<Vec<_>>();
                (200, json_headers(), json_body(json!({ "revision": revision, "files": files })))
            }
            _ if method == "GET" && path.contains("/blobs/") => {
                let sha256 = last_segment(path).to_owned();
                let read = match url.query() {
                    Some(query) => format!("{sha256}?{query}"),
                    None => sha256.clone(),
                };
                self.blob_reads.lock().expect("the blob reads lock is never poisoned").push(read);
                let bytes = self
                    .blobs
                    .lock()
                    .expect("the blobs lock is never poisoned")
                    .get(&sha256)
                    .cloned()
                    .unwrap_or_default();
                (200, Vec::new(), bytes)
            }
            _ if method == "PUT" && path.starts_with("/api/artifacts/") => {
                let slug = last_segment(path).to_owned();
                let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
                let url = format!("https://opencode.ai/console/org_1/artifacts/{slug}");
                self.published.lock().expect("the published lock is never poisoned").push((slug, body));
                (200, json_headers(), json_body(json!({ "url": url, "watching": false })))
            }
            _ if method == "DELETE" && path.starts_with("/api/artifacts/") => {
                let slug = last_segment(path).to_owned();
                self.deleted.lock().expect("the deleted lock is never poisoned").push(slug);
                (204, Vec::new(), Bytes::new())
            }
            _ => not_found(),
        }
    }
}

impl HttpSender for Console {
    fn send(&self, request: PreparedHttp) -> BoxFuture<Result<HttpResponse, AiError>> {
        let (status, headers, body) = self.respond(&request);
        Box::pin(async move {
            Ok(HttpResponse { status, headers, body: Box::pin(futures_util::stream::once(async move { Ok(body) })) })
        })
    }
}

type Reply = (u16, Vec<(String, String)>, Bytes);

fn not_found() -> Reply {
    (404, Vec::new(), Bytes::from_static(b"Not found"))
}

fn json_headers() -> Vec<(String, String)> {
    vec![("content-type".to_owned(), "application/json".to_owned())]
}

fn json_body(value: Value) -> Bytes {
    Bytes::from(serde_json::to_vec(&value).expect("a fake Console response serializes"))
}

fn last_segment(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or_default()
}

async fn register(console: Arc<Console>) -> (Harness, PluginState) {
    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential("opencode", credential()).await;
    let http: Arc<dyn HttpSender> = console;
    let state = plugin_context_state(&harness, http);
    let mut builtin = Builtin::default();
    opencode::register(&harness.services, &state.context, &mut builtin).await;
    harness.record(builtin);
    (harness, state)
}

async fn start_console(artifacts: bool, watching: bool) -> (Arc<Console>, Harness, PluginState) {
    let console = Arc::new(Console::default());
    console.set_artifacts(artifacts);
    console.set_watches(watching);
    let (harness, state) = register(console.clone()).await;
    (console, harness, state)
}

fn artifacts(state: &PluginState) -> opencode::ConsoleArtifacts {
    state.context.console.artifacts().expect("the Console artifact provider is offered")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adds_no_console_provider_while_the_console_config_does_not_advertise_artifacts() {
    let (_console, _harness, state) = start_console(false, false).await;
    assert!(state.context.console.artifacts().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn makes_console_the_default_provider_while_advertised_and_removes_it_when_the_header_goes() {
    let (console, harness, state) = start_console(true, true).await;
    let provider = artifacts(&state);
    assert_eq!(
        provider.provider(),
        ArtifactProvider {
            id: "console".to_owned(),
            name: "OpenCode Console".to_owned(),
            capabilities: vec![ArtifactCapability::Static],
            default: true,
        }
    );
    assert!(provider.prompt().contains("Comments wake this session."));

    console.set_artifacts(false);
    opencode::refresh(&harness.services, &state.context, Duration::ZERO).await;
    assert!(state.context.console.artifacts().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publishes_with_the_session_and_an_update_keeps_the_slug_and_uploads_only_new_files() {
    let (console, _harness, state) = start_console(true, true).await;
    let provider = artifacts(&state);
    let id = "art_ABC123";
    let session = "ses_1";
    let url = provider
        .publish(id, "Q3 Report!", files(), Some(session.to_owned()), false)
        .await
        .expect("the artifact publishes");
    let published = console.published();
    let slug = published[0].0.clone();
    assert_eq!(slug, "q3-report-abc123");
    assert_eq!(url, format!("https://opencode.ai/console/org_1/artifacts/{slug}"));
    assert_eq!(console.uploads().len(), 2);
    assert!(console.prepared()[0].get("replace").is_none());
    assert_eq!(published[0].1["title"], json!("Q3 Report!"));
    assert_eq!(published[0].1["sessionID"], json!(session));

    let mut updated_files = files();
    updated_files.insert("index.html".to_owned(), Bytes::from("<h1>Bye</h1>"));
    let updated = provider.publish(id, "Q4 Report", updated_files, None, true).await.expect("the artifact updates");
    assert_eq!(updated, format!("https://opencode.ai/console/org_1/artifacts/{slug}"));
    assert_eq!(console.uploads().len(), 3);
    assert_eq!(console.prepared()[1]["slug"], json!(slug));
    assert_eq!(console.prepared()[1]["replace"], json!(true));
    assert_eq!(console.published()[1].1["title"], json!("Q4 Report"));
    assert_eq!(console.published()[1].1["replace"], json!(true));

    provider.delete(id).await.expect("the artifact is deleted");
    assert_eq!(console.deleted(), vec![slug]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reads_the_files_back_downloading_the_ones_console_does_not_inline_from_the_listed_revision() {
    let (console, _harness, state) = start_console(true, true).await;
    let provider = artifacts(&state);
    provider.publish("art_ABC123", "Report", files(), None, false).await.expect("the artifact publishes");
    let content = provider.read("art_ABC123").await.expect("the artifact reads back");
    assert_eq!(content, files());
    let sha256 = opencode_util::hash::sha256_hex(b"console.log(1)");
    assert_eq!(console.blob_reads(), vec![format!("{sha256}?revision=1")]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn says_console_has_no_such_artifact_when_it_was_deleted_there() {
    let (console, _harness, state) = start_console(true, true).await;
    let provider = artifacts(&state);
    provider.publish("art_ABC123", "Report", files(), None, false).await.expect("the artifact publishes");
    console.set_gone(true);
    let error = provider.read("art_ABC123").await.expect_err("the deleted artifact cannot be read");
    let slug = &console.published()[0].0;
    assert!(error.contains(&format!("OpenCode Console has no artifact named {slug}")), "got {error}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gives_two_artifacts_with_one_title_their_own_console_artifacts() {
    let (console, _harness, state) = start_console(true, true).await;
    let provider = artifacts(&state);
    provider.publish("art_AAAAAA", "Report", files(), None, false).await.expect("the first artifact publishes");
    provider.publish("art_BBBBBB", "Report", files(), None, false).await.expect("the second artifact publishes");
    let slugs: Vec<String> = console.published().into_iter().map(|(slug, _)| slug).collect();
    assert_eq!(slugs, vec!["report-aaaaaa".to_owned(), "report-bbbbbb".to_owned()]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_event_watches_publishes_without_the_session_and_promises_no_wake() {
    let (console, _harness, state) = start_console(true, false).await;
    let provider = artifacts(&state);
    assert!(!provider.prompt().contains("wake"));
    provider
        .publish("art_ABC123", "Demo", files(), Some("ses_1".to_owned()), false)
        .await
        .expect("the artifact publishes");
    assert!(console.published()[0].1.get("sessionID").is_none());
}
