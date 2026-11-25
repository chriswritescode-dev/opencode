use bytes::Bytes;
use http::{Request, StatusCode};
use opencode_plugin::generated::builtins::{POST, PRE};
use serde_json::Value;
use tokio::time::{Duration, Instant, sleep};

use crate::{authorized, basic, ready_app_with_services, services_at, temp_directory, text};

fn json_post(uri: &str, body: &str) -> Request<Bytes> {
    let credential = basic("opencode", "pw");
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("authorization", credential)
        .header("content-type", "application/json")
        .body(Bytes::from(body.to_owned()))
        .expect("a request")
}

async fn body_json(response: opencode_http::response::Response) -> Value {
    serde_json::from_str(&text(response).await).expect("a JSON body")
}

#[tokio::test]
async fn lists_the_builtins_in_upstream_order() {
    let start = temp_directory("plugin-list-start");
    let app = ready_app_with_services(services_at(&start));
    let deadline = Instant::now() + Duration::from_secs(5);
    let body = loop {
        let response = app.handle(authorized("GET", "/api/plugin")).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        if body["data"].as_array().is_some_and(|data| data.len() == PRE.len() + POST.len()) {
            break body;
        }
        assert!(Instant::now() < deadline, "plugin inventory did not settle: {body}");
        sleep(Duration::from_millis(10)).await;
    };
    let data = body["data"].as_array().expect("plugin data");
    let expected: Vec<&str> = PRE.iter().chain(POST.iter()).copied().collect();
    let ids: Vec<&str> = data.iter().map(|item| item["id"].as_str().expect("a plugin id")).collect();
    assert_eq!(ids, expected);
    for item in data {
        assert_eq!(item["source"]["type"], "builtin");
        assert_eq!(item["state"]["status"], "active");
        assert_eq!(item["features"], serde_json::json!({ "server": true }));
    }
    assert!(body["location"]["directory"].is_string());
}

#[tokio::test]
async fn gui_of_an_unknown_id_is_not_found() {
    let start = temp_directory("plugin-gui-start");
    let app = ready_app_with_services(services_at(&start));
    let response = app.handle(authorized("GET", "/api/plugin/gui?id=missing")).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = body_json(response).await;
    assert_eq!(body["_tag"], "PluginNotFoundError");
    assert_eq!(body["pluginID"], "missing");
    assert_eq!(body["message"], "No active plugin with a ./gui-experimental bundle: missing");
}

#[tokio::test]
async fn check_of_an_unknown_target_is_invalid() {
    let start = temp_directory("plugin-check-start");
    let app = ready_app_with_services(services_at(&start));
    let response = app.handle(json_post("/api/plugin/check", r#"{"target":"nope"}"#)).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(body["_tag"], "InvalidRequestError");
    assert_eq!(body["field"], "target");
    assert_eq!(body["message"], "Plugin package is not in the current server inventory: nope");
}

#[tokio::test]
async fn update_of_unknown_targets_is_invalid() {
    let start = temp_directory("plugin-update-start");
    let app = ready_app_with_services(services_at(&start));
    let response = app.handle(json_post("/api/plugin/update", r#"{"targets":["nope","other"]}"#)).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(body["_tag"], "InvalidRequestError");
    assert_eq!(body["field"], "targets");
    assert_eq!(body["message"], "Plugin packages are not in the current server inventory: nope, other");
}

#[tokio::test]
async fn a_location_boot_publishes_plugin_updated() {
    let start = temp_directory("plugin-event-start");
    let services = services_at(&start);
    let mut events = services.bus.subscribe(&["plugin.updated"], None);
    let app = ready_app_with_services(services);
    let response = app.handle(authorized("GET", "/api/plugin")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let deadline = Instant::now() + Duration::from_secs(5);
    let event = loop {
        if let Some(event) = events.try_recv() {
            break event;
        }
        assert!(Instant::now() < deadline, "the location boot did not publish plugin.updated");
        sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(event.r#type(), "plugin.updated");
}
