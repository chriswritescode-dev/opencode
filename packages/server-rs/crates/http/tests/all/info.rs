use http::StatusCode;
use opencode_http::app::App;
use serde_json::{Value, json};

use crate::{UNAUTHORIZED_BODY, app, authorized, ready_app, request, text};

const READY_KEYS: [&str; 6] = ["version", "pid", "hostname", "urls", "paths", "capabilities"];
const UNAVAILABLE_KEYS: [&str; 4] = ["version", "pid", "urls", "paths"];

fn assert_keys_in_order(body: &str, keys: &[&str]) {
    let mut previous = None;
    for key in keys {
        let position = body.find(&format!("\"{key}\":")).expect("a key in the body");
        assert!(previous.is_none_or(|previous| previous < position), "{key} is out of order");
        previous = Some(position);
    }
}

fn assert_unavailable_shape(body: &str) -> Value {
    assert_keys_in_order(body, &UNAVAILABLE_KEYS);
    let value: Value = serde_json::from_str(body).expect("json");
    assert!(value.get("hostname").is_none());
    assert!(value.get("capabilities").is_none());
    value
}

async fn unavailable_info(app: &App) -> String {
    let response = app.handle(authorized("GET", "/api/info")).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers().get("retry-after").and_then(|value| value.to_str().ok()), Some("1"));
    text(response).await
}

#[tokio::test]
async fn ready_info_has_the_typescript_key_order() {
    let app = ready_app();
    let response = app.handle(authorized("GET", "/api/info")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("content-type").and_then(|value| value.to_str().ok()), Some("application/json"));
    let body = text(response).await;
    assert_keys_in_order(&body, &READY_KEYS);
    let value: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(value["version"], json!("test-version"));
    assert_eq!(value["pid"].as_u64(), Some(u64::from(std::process::id())));
    assert_eq!(value["urls"], json!(["http://127.0.0.1:4096"]));
    assert_eq!(value["paths"], json!({ "tmp": "/tmp/opencode-http-test" }));
    assert_eq!(value["capabilities"], json!({ "persistentPty": false }));
    assert!(value["hostname"].as_str().is_some_and(|hostname| !hostname.is_empty()));
}

#[tokio::test]
async fn starting_and_stopping_info_are_503_with_retry_after() {
    let app = app();
    let starting = unavailable_info(&app).await;
    assert_unavailable_shape(&starting);
    app.status().begin_stopping();
    let stopping = unavailable_info(&app).await;
    assert_unavailable_shape(&stopping);
}

#[tokio::test]
async fn failed_info_is_500_without_retry_after() {
    let app = app();
    app.status().fail();
    let response = app.handle(authorized("GET", "/api/info")).await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(response.headers().get("retry-after").is_none());
    let body = text(response).await;
    let value = assert_unavailable_shape(&body);
    assert_eq!(value["version"], json!("test-version"));
}

#[tokio::test]
async fn unauthorized_info_is_401_before_readiness() {
    let app = app();
    let response = app.handle(request("GET", "/api/info")).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(text(response).await, UNAUTHORIZED_BODY);
}
