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
        crate::offline_env(),
        state.bus.clone(),
        crate::global_plugins(),
        "test-version".to_owned(),
        "test".to_owned(),
        Arc::new(Vec::new()),
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

async fn boot(services: &GlobalServices, directory: &Path) -> String {
    let reference = opencode_location::location::LocationRef::new(directory.to_string_lossy().into_owned(), None);
    let lease = services.locations.get(&reference).await.expect("boot the location");
    lease.services().info.project.id.clone()
}

fn seed(state: &ServerState, session_id: &str, directory: &Path, project_id: &str) {
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
}

fn text_form(id: &str) -> Value {
    json!({
        "id": id,
        "title": "Title",
        "fields": [{ "type": "string", "key": "name" }],
    })
}

#[tokio::test]
async fn form_routes_work_for_the_global_sentinel() {
    let start = temp_directory("form-global-start");
    let home = temp_directory("form-global-home");
    let state = state();
    let services = services(&state, &start, &home);
    let app = app(&state, services);

    let response = app.handle(json_request("POST", "/api/session/global/form", &text_form("frm_one"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["data"]["id"], "frm_one");
    assert_eq!(body["data"]["sessionID"], "global");
    assert_eq!(body["data"]["fields"][0]["key"], "name");

    let response = app.handle(authorized("GET", "/api/session/global/form")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["data"].as_array().expect("forms").len(), 1);
    assert_eq!(body["data"][0]["id"], "frm_one");

    let response = app.handle(authorized("GET", "/api/session/global/form/frm_one")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["data"]["state"]["status"], "pending");

    let response = app
        .handle(json_request("POST", "/api/session/global/form/frm_one/reply", &json!({"answer": {"name": "Ada"}})))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let response = app.handle(authorized("GET", "/api/session/global/form/frm_one")).await;
    let body = json(response).await;
    assert_eq!(body["data"]["state"]["status"], "answered");
    assert_eq!(body["data"]["state"]["answer"]["name"], "Ada");

    let response = app
        .handle(json_request("POST", "/api/session/global/form/frm_one/reply", &json!({"answer": {"name": "Ada"}})))
        .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = json(response).await;
    assert_eq!(body["_tag"], "FormAlreadySettledError");
    assert_eq!(body["id"], "frm_one");
    assert_eq!(body["message"], "Form already settled: frm_one");

    let response = app.handle(json_request("POST", "/api/session/global/form", &text_form("frm_two"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = app.handle(authorized("DELETE", "/api/session/global/form/frm_two?message=stop")).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response = app.handle(authorized("GET", "/api/session/global/form/frm_two")).await;
    let body = json(response).await;
    assert_eq!(body["data"]["state"]["status"], "cancelled");
    assert_eq!(body["data"]["state"]["message"], "stop");

    let response = app.handle(json_request("POST", "/api/session/global/form", &text_form("frm_one"))).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = json(response).await;
    assert_eq!(body["_tag"], "ConflictError");
    assert_eq!(body["resource"], "frm_one");
    assert_eq!(body["message"], "Form already exists: frm_one");

    let duplicate = json!({
        "title": "Title",
        "fields": [
            { "type": "string", "key": "name" },
            { "type": "string", "key": "name" },
        ],
    });
    let response = app.handle(json_request("POST", "/api/session/global/form", &duplicate)).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json(response).await;
    assert_eq!(body["_tag"], "InvalidRequestError");
    assert_eq!(body["field"], "fields");
    assert_eq!(body["message"], "Duplicate form field key: name");

    let required = json!({
        "id": "frm_required",
        "title": "Title",
        "fields": [{ "type": "string", "key": "name", "required": true }],
    });
    let response = app.handle(json_request("POST", "/api/session/global/form", &required)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let response =
        app.handle(json_request("POST", "/api/session/global/form/frm_required/reply", &json!({"answer": {}}))).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json(response).await;
    assert_eq!(body["_tag"], "FormInvalidAnswerError");
    assert_eq!(body["id"], "frm_required");
    assert_eq!(body["message"], "Missing required form field: name");

    let response = app.handle(authorized("GET", "/api/session/global/form/frm_missing")).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = json(response).await;
    assert_eq!(body["_tag"], "FormNotFoundError");
    assert_eq!(body["id"], "frm_missing");
    assert_eq!(body["message"], "Form not found: frm_missing");

    let response = app.handle(authorized("GET", &location_uri("/api/form", &start))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["location"]["directory"], start.to_string_lossy().as_ref());
    let ids = body["data"]
        .as_array()
        .expect("forms")
        .iter()
        .map(|form| form["id"].as_str().expect("a form id"))
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["frm_required"]);
}

#[tokio::test]
async fn form_answers_of_every_value_kind_round_trip_through_get() {
    let start = temp_directory("form-kinds-start");
    let home = temp_directory("form-kinds-home");
    let state = state();
    let services = services(&state, &start, &home);
    let app = app(&state, services);

    let form = json!({
        "id": "frm_kinds",
        "title": "Every kind",
        "metadata": { "nested": { "list": [1, null, true], "text": "value" } },
        "fields": [
            { "type": "string", "key": "date", "format": "date" },
            { "type": "string", "key": "timestamp", "format": "date-time" },
            { "type": "string", "key": "sentinel" },
            { "type": "number", "key": "number", "minimum": 0, "maximum": 100, "default": 1.5 },
            { "type": "integer", "key": "integer", "maximum": 10 },
            { "type": "boolean", "key": "boolean", "default": false },
            {
                "type": "multiselect",
                "key": "multi",
                "options": [{ "value": "a", "label": "A" }, { "value": "b", "label": "B" }],
                "minItems": 1
            },
            { "type": "external", "key": "external", "url": "https://example.test/form" },
            {
                "type": "string",
                "key": "string_branch",
                "when": [{ "key": "date", "op": "eq", "value": "2026-11-25" }]
            },
            {
                "type": "string",
                "key": "number_branch",
                "when": [{ "key": "number", "op": "eq", "value": 42.5 }]
            },
            {
                "type": "string",
                "key": "boolean_branch",
                "when": [{ "key": "boolean", "op": "eq", "value": true }]
            }
        ]
    });
    let response = app.handle(json_request("POST", "/api/session/global/form", &form)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let created = json(response).await;
    assert_eq!(created["data"]["metadata"], form["metadata"]);
    assert_eq!(created["data"]["fields"], form["fields"]);

    let answer = json!({
        "date": "2026-11-25",
        "timestamp": "2026-11-25T10:30:00Z",
        "sentinel": "Infinity",
        "number": 42.5,
        "integer": 7,
        "boolean": true,
        "multi": ["a", "b"],
        "external": true,
        "string_branch": "matched",
        "number_branch": "matched",
        "boolean_branch": "matched"
    });
    let response = app
        .handle(json_request("POST", "/api/session/global/form/frm_kinds/reply", &json!({ "answer": answer })))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let response = app.handle(authorized("GET", "/api/session/global/form/frm_kinds")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["data"]["metadata"], form["metadata"]);
    assert_eq!(body["data"]["fields"], form["fields"]);
    assert_eq!(body["data"]["state"]["status"], "answered");
    assert_eq!(body["data"]["state"]["answer"], answer);
}

#[tokio::test]
async fn form_routes_use_the_session_location() {
    let start = temp_directory("form-session-start");
    let home = temp_directory("form-session-home");
    let far = temp_directory("form-session-far");
    let state = state();
    let services = services(&state, &start, &home);
    let app = app(&state, services.clone());
    let project_id = boot(&services, &start).await;
    seed(&state, "ses_1", &start, &project_id);
    seed(&state, "ses_far", &far, &project_id);

    let response = app.handle(authorized("GET", "/api/session/ses_1/form")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json(response).await["data"], json!([]));

    let response = app.handle(json_request("POST", "/api/session/ses_1/form", &text_form("frm_session"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json(response).await["data"]["sessionID"], "ses_1");

    let response = app.handle(authorized("GET", "/api/session/ses_1/form")).await;
    let body = json(response).await;
    assert_eq!(body["data"][0]["id"], "frm_session");

    let response = app.handle(authorized("GET", "/api/session/global/form/frm_session")).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(json(response).await["_tag"], "FormNotFoundError");

    let response = app.handle(authorized("GET", "/api/session/ses_missing/form")).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = json(response).await;
    assert_eq!(body["_tag"], "SessionNotFoundError");
    assert_eq!(body["sessionID"], "ses_missing");

    let far_reference = opencode_location::location::LocationRef::new(far.to_string_lossy().into_owned(), None);
    assert!(!services.locations.has(&far_reference).await);
    let response = app.handle(authorized("GET", "/api/session/ses_far/form")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json(response).await["data"], json!([]));
    assert!(!services.locations.has(&far_reference).await);
}
