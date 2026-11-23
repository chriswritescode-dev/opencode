mod auth;
mod info;
mod router;
mod serve;
mod status;

use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use http::Request;
use opencode_http::app::{App, Options};

const UNAUTHORIZED_BODY: &str = r#"{"_tag":"UnauthorizedError","message":"Authentication required"}"#;

fn app() -> Arc<App> {
    App::new(Options {
        password: "pw".to_owned(),
        version: "test-version".to_owned(),
        cors: Vec::new(),
        tmp: PathBuf::from("/tmp/opencode-http-test"),
        hostname: None,
        urls: Arc::new(|| vec!["http://127.0.0.1:4096".to_owned()]),
    })
}

fn ready_app() -> Arc<App> {
    let app = app();
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

async fn text(response: http::Response<http_body_util::Full<Bytes>>) -> String {
    use http_body_util::BodyExt;
    let bytes = response.into_body().collect().await.expect("a body").to_bytes();
    String::from_utf8(bytes.to_vec()).expect("utf-8")
}
