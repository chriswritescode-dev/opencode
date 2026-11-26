use std::collections::HashMap;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use http_body_util::Full;
use hyper::Request;
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use opencode_registry::skill::discovery::{cache_directory, pull as pull_source};
use serde_json::Value;
use tokio::net::TcpListener;

use crate::TempDir;

#[derive(Default)]
struct State {
    skills: Vec<Value>,
    files: HashMap<String, String>,
    requests: Vec<String>,
}

async fn serve() -> (String, Arc<Mutex<State>>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind the skill server");
    let address = listener.local_addr().expect("the skill server address");
    let state = Arc::new(Mutex::new(State::default()));
    let shared = state.clone();
    let server = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { return };
            let state = shared.clone();
            tokio::spawn(async move {
                let service = service_fn(move |request: Request<Incoming>| {
                    let state = state.clone();
                    async move { respond(&state, &address, request.uri()) }
                });
                let _ = http1::Builder::new().serve_connection(TokioIo::new(stream), service).await;
            });
        }
    });
    (format!("http://{address}/catalog/"), state, server)
}

fn respond(
    state: &Arc<Mutex<State>>,
    address: &SocketAddr,
    uri: &hyper::Uri,
) -> Result<hyper::Response<Full<Bytes>>, std::convert::Infallible> {
    let mut state = state.lock().expect("the skill server state lock is not poisoned");
    state.requests.push(format!("http://{address}{uri}"));
    let body = if uri.path() == "/catalog/index.json" {
        Some(serde_json::json!({ "skills": state.skills }).to_string())
    } else {
        state.files.get(uri.path()).cloned()
    };
    let (status, body) = match body {
        Some(body) => (200, body),
        None => (404, "Not Found".to_string()),
    };
    let response = hyper::Response::builder()
        .status(status)
        .body(Full::new(Bytes::from(body)))
        .expect("the skill server response builds");
    Ok(response)
}

async fn pull(
    base: &str,
    cache: &Path,
    state: &Arc<Mutex<State>>,
    skills: Vec<Value>,
    files: &[(&str, &str)],
) -> (Vec<PathBuf>, Vec<String>) {
    {
        let mut state = state.lock().expect("the skill server state lock is not poisoned");
        state.skills = skills;
        state.files = files.iter().map(|(path, body)| (path.to_string(), body.to_string())).collect();
        state.requests.clear();
    }
    let directories = pull_source(base, cache).await;
    let requests = state.lock().expect("the skill server state lock is not poisoned").requests.clone();
    (directories, requests)
}

async fn assert_rejected(name: &str, skill: Value) {
    let temp = TempDir::new();
    let (base, state, server) = serve().await;
    let (directories, requests) = pull(&base, temp.path(), &state, vec![skill], &[]).await;
    assert!(directories.is_empty(), "case {name}: no skill directory is returned");
    assert_eq!(requests, [format!("{base}index.json")], "case {name}: only the index is fetched");
    let entries = fs::read_dir(temp.path()).expect("the cache is readable").collect::<Vec<_>>();
    assert!(entries.is_empty(), "case {name}: nothing is written to the cache");
    server.abort();
}

#[tokio::test]
async fn skill_discovery_rejects_skill_name_traversal_without_fetching_files() {
    assert_rejected("../outside", serde_json::json!({ "name": "../outside", "files": ["SKILL.md"] })).await;
}

#[tokio::test]
async fn skill_discovery_rejects_file_traversal_without_fetching_files() {
    assert_rejected("file-traversal", serde_json::json!({ "name": "deploy", "files": ["SKILL.md", "../outside.md"] }))
        .await;
}

#[tokio::test]
async fn skill_discovery_rejects_absolute_file_paths_without_fetching_files() {
    assert_rejected("absolute-path", serde_json::json!({ "name": "deploy", "files": ["SKILL.md", "/tmp/outside.md"] }))
        .await;
}

#[tokio::test]
async fn skill_discovery_rejects_cross_origin_file_urls_without_fetching_files() {
    assert_rejected(
        "cross-origin",
        serde_json::json!({ "name": "deploy", "files": ["SKILL.md", "https://evil.example.test/outside.md"] }),
    )
    .await;
}

#[tokio::test]
async fn skill_discovery_downloads_safe_nested_files_under_the_skill_root() {
    let temp = TempDir::new();
    let (base, state, server) = serve().await;
    let (directories, requests) = pull(
        &base,
        temp.path(),
        &state,
        vec![serde_json::json!({ "name": "deploy", "files": ["SKILL.md", "references/guide.md"] })],
        &[("/catalog/deploy/SKILL.md", "# Deploy"), ("/catalog/deploy/references/guide.md", "# Guide")],
    )
    .await;

    assert_eq!(directories.len(), 1, "the safe skill is published");
    let mut actual = requests.clone();
    let mut expected = vec![
        format!("{base}index.json"),
        format!("{base}deploy/SKILL.md"),
        format!("{base}deploy/references/guide.md"),
    ];
    actual.sort();
    expected.sort();
    assert_eq!(actual, expected, "the index and both safe files are fetched");

    let root = &directories[0];
    assert_eq!(fs::read_to_string(root.join("SKILL.md")).expect("the skill document is readable"), "# Deploy");
    assert_eq!(fs::read_to_string(root.join("references/guide.md")).expect("the nested file is readable"), "# Guide");
    server.abort();
}

#[tokio::test]
async fn skill_discovery_refreshes_cached_files_when_the_version_changes() {
    let temp = TempDir::new();
    let (base, state, server) = serve().await;
    let files = [("/catalog/deploy/SKILL.md", "# Old")];
    let (first, _) = pull(
        &base,
        temp.path(),
        &state,
        vec![serde_json::json!({ "name": "deploy", "version": "1", "files": ["SKILL.md"] })],
        &files,
    )
    .await;

    let (_, second) = pull(
        &base,
        temp.path(),
        &state,
        vec![serde_json::json!({ "name": "deploy", "version": "2", "files": ["SKILL.md"] })],
        &[("/catalog/deploy/SKILL.md", "# New")],
    )
    .await;
    assert_eq!(
        fs::read_to_string(first[0].join("SKILL.md")).expect("the refreshed document is readable"),
        "# New",
        "the new version replaces the cached document"
    );
    assert!(second.contains(&format!("{base}deploy/SKILL.md")), "the refresh fetches the new document");

    let (_, third) = pull(
        &base,
        temp.path(),
        &state,
        vec![serde_json::json!({ "name": "deploy", "version": "2", "files": ["SKILL.md"] })],
        &[("/catalog/deploy/SKILL.md", "# Ignored")],
    )
    .await;
    assert_eq!(third, [format!("{base}index.json")], "an unchanged version fetches only the index");
    server.abort();
}

#[tokio::test]
async fn skill_discovery_publishes_complete_updates_and_removes_stale_files() {
    let temp = TempDir::new();
    let (base, state, server) = serve().await;
    let (first, _) = pull(
        &base,
        temp.path(),
        &state,
        vec![serde_json::json!({ "name": "deploy", "version": "1", "files": ["SKILL.md", "old.md"] })],
        &[("/catalog/deploy/SKILL.md", "# Old"), ("/catalog/deploy/old.md", "old reference")],
    )
    .await;
    let root = &first[0];

    pull(
        &base,
        temp.path(),
        &state,
        vec![serde_json::json!({ "name": "deploy", "version": "2", "files": ["SKILL.md", "missing.md"] })],
        &[("/catalog/deploy/SKILL.md", "# Partial")],
    )
    .await;
    assert_eq!(
        fs::read_to_string(root.join("SKILL.md")).expect("the cached document is readable"),
        "# Old",
        "an incomplete update leaves the cached document"
    );
    assert_eq!(
        fs::read_to_string(root.join("old.md")).expect("the cached reference is readable"),
        "old reference",
        "an incomplete update leaves the cached reference"
    );

    pull(
        &base,
        temp.path(),
        &state,
        vec![serde_json::json!({ "name": "deploy", "version": "3", "files": ["SKILL.md"] })],
        &[("/catalog/deploy/SKILL.md", "# New")],
    )
    .await;
    assert_eq!(
        fs::read_to_string(root.join("SKILL.md")).expect("the replaced document is readable"),
        "# New",
        "a complete update replaces the cached document"
    );
    assert!(!root.join("old.md").exists(), "a complete update removes a stale file");
    server.abort();
}

#[test]
fn skill_discovery_cache_directory_hashes_the_trailing_slash_normalized_url() {
    let temp = TempDir::new();
    for (url, suffix) in [
        ("https://example.com/skills", "skills/a78602712c933c3ebcf54e825053fc1459bb0e9a"),
        ("https://reg.example.net/catalog/", "skills/4dcaf0ab29dc06616ea8179ec22358fb26024d79"),
        ("https://skills.example.org/team/a/", "skills/c0b9a64aa785c7909f73da15cfae626badcd7a6a"),
    ] {
        let directory = cache_directory(temp.path(), url);
        assert!(directory.ends_with(suffix), "the cache directory for {url} ends with its sha1 name {suffix}");
    }
}
