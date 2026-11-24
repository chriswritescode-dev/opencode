mod auth;
mod compression;
mod cors;
mod coverage;
mod event;
mod info;
mod pairing;
mod request_site;
mod router;
mod serve;
mod session_log;
mod status;
mod ticket;

use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use http::Request;
use opencode_config::paths::GlobalPaths;
use opencode_http::app::{App, Options};
use opencode_http::services::GlobalServices;
use opencode_storage::{Database, DatabaseLocation};

const UNAUTHORIZED_BODY: &str = r#"{"_tag":"UnauthorizedError","message":"Authentication required"}"#;

fn services() -> Arc<GlobalServices> {
    let (database, _) = Database::open(DatabaseLocation::Memory, &std::env::temp_dir()).expect("open the database");
    let paths = GlobalPaths::from_vars(|name| {
        (name == "HOME").then(|| std::env::temp_dir().join("opencode-http-test-home").into_os_string())
    })
    .expect("global paths");
    Arc::new(GlobalServices::new(
        Arc::new(database),
        paths,
        "/tmp/opencode-http-test".to_owned(),
        Arc::new(|_: &str| None),
    ))
}

fn app() -> Arc<App> {
    make_app(Vec::new(), None)
}

fn app_with_cors(cors: Vec<String>) -> Arc<App> {
    make_app(cors, None)
}

fn app_with_hostname(hostname: Option<&str>) -> Arc<App> {
    make_app(Vec::new(), hostname)
}

fn make_app(cors: Vec<String>, hostname: Option<&str>) -> Arc<App> {
    App::new(Options {
        password: "pw".to_owned(),
        version: "test-version".to_owned(),
        cors,
        tmp: PathBuf::from("/tmp/opencode-http-test"),
        hostname: hostname.map(str::to_owned),
        urls: Arc::new(|| vec!["http://127.0.0.1:4096".to_owned()]),
        state: None,
        services: Some(services()),
    })
}

fn ready_app() -> Arc<App> {
    let app = app();
    app.status().ready();
    app
}

fn ready_app_with_cors(cors: Vec<String>) -> Arc<App> {
    let app = app_with_cors(cors);
    app.status().ready();
    app
}

fn request(method: &str, uri: &str) -> Request<Bytes> {
    Request::builder().method(method).uri(uri).body(Bytes::new()).expect("a request")
}

fn request_with(method: &str, uri: &str, headers: &[(&str, &str)]) -> Request<Bytes> {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder.body(Bytes::new()).expect("a request")
}

fn encoded(user: &str, password: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
}

fn basic(user: &str, password: &str) -> String {
    let credential = encoded(user, password);
    format!("Basic {credential}")
}

fn authorized(method: &str, uri: &str) -> Request<Bytes> {
    let credential = basic("opencode", "pw");
    request_with(method, uri, &[("authorization", credential.as_str())])
}

async fn text(response: opencode_http::response::Response) -> String {
    use http_body_util::BodyExt;
    let bytes = response.into_body().collect().await.expect("a body").to_bytes();
    String::from_utf8(bytes.to_vec()).expect("utf-8")
}

async fn body(response: opencode_http::response::Response) -> Bytes {
    use http_body_util::BodyExt;
    response.into_body().collect().await.expect("a body").to_bytes()
}
