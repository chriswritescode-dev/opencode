use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use http::{Request, StatusCode};
use opencode_bus::{Definition, PublishOptions};
use opencode_config::paths::GlobalPaths;
use opencode_http::app::{App, Options};
use opencode_http::services::GlobalServices;
use opencode_http::state::ServerState;
use opencode_storage::DatabaseLocation;
use serde_json::{Value, json};

use crate::{authorized, temp_directory};

fn state() -> Arc<ServerState> {
    let (state, _) = ServerState::open(DatabaseLocation::Memory, Path::new("/tmp")).expect("open the server state");
    Arc::new(state)
}

fn services(state: &ServerState, start: &Path, home: &Path) -> Arc<GlobalServices> {
    let paths =
        GlobalPaths::from_vars(|name| (name == "HOME").then(|| home.as_os_str().to_owned())).expect("global paths");
    Arc::new(GlobalServices::new(
        state.db.clone(),
        paths,
        start.to_string_lossy().into_owned(),
        Arc::new(|_: &str| None),
        state.bus.clone(),
        crate::global_plugins(),
        "test-version".to_owned(),
        "test".to_owned(),
    ))
}

fn app(state: &Arc<ServerState>, services: Arc<GlobalServices>) -> Arc<App> {
    let app = App::new(Options {
        password: "pw".to_owned(),
        version: "test-version".to_owned(),
        cors: Vec::new(),
        tmp: PathBuf::from("/tmp/opencode-http-test"),
        hostname: None,
        urls: Arc::new(Vec::new),
        state: Some(state.clone()),
        services: Some(services),
    });
    app.status().ready();
    app
}

fn location_uri(path: &str, directory: &Path) -> String {
    let encoded = url::form_urlencoded::byte_serialize(directory.to_string_lossy().as_bytes()).collect::<String>();
    format!("{path}?location%5Bdirectory%5D={encoded}")
}

fn json_request(method: &str, uri: &str, body: &Value) -> Request<Bytes> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", crate::basic("opencode", "pw"))
        .header("content-type", "application/json")
        .body(Bytes::from(body.to_string()))
        .expect("a request")
}

async fn json(response: opencode_http::response::Response) -> Value {
    serde_json::from_str(&crate::text(response).await).expect("a JSON body")
}

async fn boot_and_seed(
    services: &GlobalServices,
    state: &ServerState,
    session_id: &str,
    directory: &Path,
    project_id: Option<&str>,
) -> String {
    let reference = opencode_location::location::LocationRef::new(directory.to_string_lossy().into_owned(), None);
    let lease = services.locations.get(&reference).await.expect("boot the location");
    let project_id = project_id.map_or_else(|| lease.services().info.project.id.clone(), str::to_owned);
    let definition = Definition::latest_durable("session.created").expect("session.created is durable");
    state
        .bus
        .publish(
            &definition,
            json!({
                "sessionID": session_id,
                "projectID": project_id,
                "location": { "directory": directory.to_string_lossy() },
                "slug": session_id,
                "version": "1",
            }),
            PublishOptions { id: Some(format!("evt_{session_id}")), ..Default::default() },
        )
        .expect("publish session.created");
    project_id
}

#[tokio::test]
async fn permission_routes_follow_the_request_lifecycle() {
    let start = temp_directory("permission-start");
    let home = temp_directory("permission-home");
    let state = state();
    let services = services(&state, &start, &home);
    let app = app(&state, services.clone());
    let project_id = boot_and_seed(&services, &state, "ses_1", &start, None).await;
    boot_and_seed(&services, &state, "ses_2", &start, Some(&project_id)).await;

    let create = json!({"action": "read", "resources": ["foo.env"], "save": ["foo.env"]});
    let response = app.handle(json_request("POST", "/api/session/ses_1/permission", &create)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["data"]["effect"], "ask");
    let request_id = body["data"]["id"].as_str().expect("a request id").to_owned();
    assert!(request_id.starts_with("per_"), "{request_id}");

    let response = app.handle(authorized("GET", &location_uri("/api/permission/request", &start))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["location"]["directory"], start.to_string_lossy().as_ref());
    assert_eq!(body["data"].as_array().expect("pending requests").len(), 1);
    assert_eq!(body["data"][0]["id"], request_id.as_str());

    let response = app.handle(authorized("GET", "/api/session/ses_1/permission")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["data"][0]["id"], request_id.as_str());

    let get = format!("/api/session/ses_1/permission/{request_id}");
    let response = app.handle(authorized("GET", &get)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["data"]["id"], request_id.as_str());
    assert_eq!(body["data"]["action"], "read");

    let other = format!("/api/session/ses_2/permission/{request_id}");
    let response = app.handle(authorized("GET", &other)).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = json(response).await;
    assert_eq!(body["_tag"], "PermissionNotFoundError");
    assert_eq!(body["requestID"], request_id.as_str());
    assert_eq!(body["message"], format!("Permission request not found: {request_id}"));

    let reply = format!("/api/session/ses_1/permission/{request_id}/reply");
    let response = app.handle(json_request("POST", &reply, &json!({"decision": "once"}))).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let response = app.handle(authorized("GET", &get)).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = app.handle(json_request("POST", "/api/session/ses_1/permission", &create)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    let second_id = body["data"]["id"].as_str().expect("a request id").to_owned();
    let response = app
        .handle(json_request(
            "POST",
            &format!("/api/session/ses_1/permission/{second_id}/reply"),
            &json!({"decision": "always"}),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let response = app.handle(json_request("POST", "/api/session/ses_1/permission", &create)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["data"]["effect"], "allow");

    let saved_uri = location_uri("/api/permission/saved", &start);
    let response = app.handle(authorized("GET", &saved_uri)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    let saved = body["data"].as_array().expect("saved permissions");
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0]["action"], "read");
    assert_eq!(saved[0]["resource"], "foo.env");
    assert_eq!(saved[0]["projectID"], project_id.as_str());
    let first_saved = saved[0]["id"].as_str().expect("a saved id").to_owned();

    let second = json!({"action": "read", "resources": ["bar.env"], "save": ["bar.env"]});
    let response = app.handle(json_request("POST", "/api/session/ses_1/permission", &second)).await;
    let body = json(response).await;
    let third_id = body["data"]["id"].as_str().expect("a request id").to_owned();
    let response = app
        .handle(json_request(
            "POST",
            &format!("/api/session/ses_1/permission/{third_id}/reply"),
            &json!({"decision": "always"}),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let response = app.handle(authorized("GET", &saved_uri)).await;
    let body = json(response).await;
    let saved = body["data"].as_array().expect("saved permissions");
    assert_eq!(saved.len(), 2);
    let second_saved = saved.iter().find(|row| row["resource"] == "bar.env").expect("the second saved row")["id"]
        .as_str()
        .expect("a saved id")
        .to_owned();

    let response = app
        .handle(json_request(
            "PATCH",
            &format!("/api/permission/saved/{second_saved}"),
            &json!({"resource": "foo.env"}),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = json(response).await;
    assert_eq!(body["_tag"], "ConflictError");
    assert_eq!(body["message"], "A saved permission already allows read for foo.env");
    assert_eq!(body["resource"], second_saved.as_str());

    let response = app
        .handle(json_request(
            "PATCH",
            &format!("/api/permission/saved/{first_saved}"),
            &json!({"action": "read", "resource": "foo.txt"}),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["data"]["resource"], "foo.txt");

    let response =
        app.handle(json_request("PATCH", "/api/permission/saved/psv_missing", &json!({"resource": "foo.env"}))).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = json(response).await;
    assert_eq!(body["_tag"], "PermissionSavedNotFoundError");
    assert_eq!(body["id"], "psv_missing");
    assert_eq!(body["message"], "Saved permission not found: psv_missing");

    let response = app.handle(authorized("DELETE", &format!("/api/permission/saved/{second_saved}"))).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let response = app.handle(authorized("GET", &saved_uri)).await;
    let body = json(response).await;
    let saved = body["data"].as_array().expect("saved permissions");
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0]["resource"], "foo.txt");
}

#[tokio::test]
async fn permission_routes_report_a_missing_session() {
    let start = temp_directory("permission-missing-start");
    let home = temp_directory("permission-missing-home");
    let state = state();
    let services = services(&state, &start, &home);
    let app = app(&state, services);

    let response = app.handle(authorized("GET", "/api/session/ses_missing/permission")).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = json(response).await;
    assert_eq!(body["_tag"], "SessionNotFoundError");
    assert_eq!(body["sessionID"], "ses_missing");
    assert_eq!(body["message"], "Session not found: ses_missing");
}
