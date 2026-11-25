use std::sync::Arc;

use base64::Engine;
use bytes::Bytes;
use http::{Request, StatusCode};
use opencode_bus::{Definition, PublishOptions};
use opencode_contract::request::RawRequest;
use opencode_http::app::App;
use serde_json::{Value, json};

use crate::{basic, session_app, temp_directory, text};

fn json_request(method: &str, uri: &str, payload: &str) -> Request<Bytes> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", basic("opencode", "pw"))
        .header("content-type", "application/json")
        .body(Bytes::from(payload.to_owned()))
        .expect("a request")
}

async fn json_body(response: opencode_http::response::Response) -> Value {
    serde_json::from_str(&text(response).await).expect("a JSON body")
}

async fn create(app: &Arc<App>, payload: &str) -> Value {
    let response = app.handle(json_request("POST", "/api/session", payload)).await;
    assert_eq!(response.status(), StatusCode::OK);
    json_body(response).await
}

async fn get(app: &Arc<App>, id: &str) -> Value {
    let response = app.handle(json_request("GET", &format!("/api/session/{id}"), "")).await;
    assert_eq!(response.status(), StatusCode::OK);
    json_body(response).await
}

fn session_id(body: &Value) -> String {
    body["data"]["id"].as_str().expect("a session id").to_owned()
}

fn titles(body: &Value) -> Vec<String> {
    body["data"]
        .as_array()
        .expect("an array of sessions")
        .iter()
        .map(|session| session["title"].as_str().expect("a session title").to_owned())
        .collect()
}

fn cursor_json(raw: &str) -> Value {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(raw).expect("a base64url cursor");
    serde_json::from_slice(&bytes).expect("a cursor JSON body")
}

async fn prompt(app: &Arc<App>, session: &str, payload: Value) -> Value {
    let response =
        app.handle(json_request("POST", &format!("/api/session/{session}/prompt"), &payload.to_string())).await;
    assert_eq!(response.status(), StatusCode::OK);
    json_body(response).await
}

async fn inbox(app: &Arc<App>, session: &str) -> Value {
    let response = app.handle(json_request("GET", &format!("/api/session/{session}/inbox"), "")).await;
    assert_eq!(response.status(), StatusCode::OK);
    json_body(response).await
}

async fn messages(app: &Arc<App>, session: &str, query: &str) -> Value {
    let response = app.handle(json_request("GET", &format!("/api/session/{session}/message{query}"), "")).await;
    assert_eq!(response.status(), StatusCode::OK);
    json_body(response).await
}

fn message_ids(body: &Value) -> Vec<String> {
    body["data"]
        .as_array()
        .expect("a message list")
        .iter()
        .map(|message| message["id"].as_str().expect("a message id").to_owned())
        .collect()
}

fn deliver(state: &opencode_http::state::ServerState, session: &str, inbox: &str) {
    let definition = Definition::latest_durable("session.inbox.delivered").expect("a durable definition");
    state
        .bus
        .publish(&definition, json!({ "sessionID": session, "inboxID": inbox }), PublishOptions::default())
        .expect("a session.inbox.delivered event");
}

#[tokio::test]
async fn create_projects_the_session_and_publishes_one_durable_created_event() {
    let start = temp_directory("session-create-start");
    let (app, state) = session_app(&start);
    let mut events = state.bus.subscribe(&["session.created"], None);

    let response = app.handle(json_request("POST", "/api/session", "{}")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("content-type").and_then(|value| value.to_str().ok()), Some("application/json"));
    let body = json_body(response).await;
    let data = &body["data"];
    let id = session_id(&body);
    assert!(id.starts_with("ses_"));
    assert!(data["projectID"].is_string());
    assert_eq!(data["location"]["directory"], start.to_string_lossy().as_ref());
    assert!(data["parentID"].is_null());
    assert!(data["title"].is_null());
    assert!(data["subpath"].is_null());
    assert!(data["metadata"].is_null());
    assert!(data["permissions"].is_null());
    assert!(data["time"]["created"].as_f64().is_some());
    assert!(data["time"]["updated"].as_f64().is_some());

    let event = events.try_recv().expect("a session.created event");
    assert_eq!(event.r#type(), "session.created");
    assert_eq!(event.durable().expect("a durable event").aggregate_id, id);
    assert_eq!(event.data()["sessionID"], id.as_str());
    assert!(events.try_recv().is_none());

    let repeated = create(&app, &json!({ "id": id, "title": "other" }).to_string()).await;
    assert_eq!(session_id(&repeated), id);
    assert_eq!(repeated["data"]["title"], data["title"]);
    assert!(events.try_recv().is_none());
}

#[tokio::test]
async fn list_returns_newest_first_and_pages_with_next_cursor() {
    let start = temp_directory("session-list-start");
    let (app, _) = session_app(&start);
    for title in ["one", "two", "three"] {
        create(&app, &json!({ "title": title }).to_string()).await;
    }

    let body = json_body(app.handle(json_request("GET", "/api/session", "")).await).await;
    assert_eq!(titles(&body), vec!["three", "two", "one"]);
    assert!(body["cursor"]["previous"].is_string());
    assert!(body["cursor"]["next"].is_string());

    let body = json_body(app.handle(json_request("GET", "/api/session?limit=2", "")).await).await;
    assert_eq!(titles(&body), vec!["three", "two"]);
    let next = body["cursor"]["next"].as_str().expect("a next cursor").to_owned();

    let encoded = url::form_urlencoded::byte_serialize(next.as_bytes()).collect::<String>();
    let body =
        json_body(app.handle(json_request("GET", &format!("/api/session?limit=2&cursor={encoded}"), "")).await).await;
    assert_eq!(titles(&body), vec!["one"]);

    let body = json_body(app.handle(json_request("GET", "/api/session?limit=1&order=asc", "")).await).await;
    assert_eq!(titles(&body), vec!["one"]);

    let response = app.handle(json_request("GET", "/api/session?cursor=not-a-cursor", "")).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json_body(response).await, json!({ "_tag": "InvalidCursorError", "message": "Invalid cursor" }));
}

#[tokio::test]
async fn list_cursor_encodes_root_parent_and_strips_union_fields() {
    let start = temp_directory("session-cursor-start");
    let (app, _) = session_app(&start);
    let parent = session_id(&create(&app, &json!({ "title": "parent" }).to_string()).await);
    create(&app, &json!({ "title": "child", "parentID": parent }).to_string()).await;
    let other = session_id(&create(&app, &json!({ "title": "other" }).to_string()).await);

    let body = json_body(app.handle(json_request("GET", "/api/session?parentID=null&limit=1", "")).await).await;
    let roots = body["data"].as_array().expect("an array of sessions");
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0]["id"], other.as_str());
    let next = body["cursor"]["next"].as_str().expect("a next cursor").to_owned();
    assert_eq!(cursor_json(&next)["parentID"], "null");

    let encoded = url::form_urlencoded::byte_serialize(next.as_bytes()).collect::<String>();
    let body =
        json_body(app.handle(json_request("GET", &format!("/api/session?limit=1&cursor={encoded}"), "")).await).await;
    let page = body["data"].as_array().expect("an array of sessions");
    assert_eq!(page.len(), 1);
    assert_eq!(page[0]["id"], parent.as_str());
    assert!(!titles(&body).contains(&"child".to_owned()));

    let raw = json!({
        "anchor": { "id": parent, "time": 0.0, "direction": "next" },
        "parentID": null,
    })
    .to_string();
    let null_parent_cursor = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
    let response = app.handle(json_request("GET", &format!("/api/session?cursor={null_parent_cursor}"), "")).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json_body(response).await, json!({ "_tag": "InvalidCursorError", "message": "Invalid cursor" }));

    let directory = temp_directory("session-cursor-directory");
    let (app, _) = session_app(&directory);
    let project = create(&app, "{}").await["data"]["projectID"].as_str().expect("a project id").to_owned();
    let query = |value: &str| url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>();
    let uri = format!("/api/session?directory={}&project={}", query(&directory.to_string_lossy()), query(&project));
    let body = json_body(app.handle(json_request("GET", &uri, "")).await).await;
    let filtered = body["data"].as_array().expect("an array of sessions");
    assert_eq!(filtered.len(), 1);
    let decoded = cursor_json(body["cursor"]["next"].as_str().expect("a next cursor"));
    assert_eq!(decoded["directory"], directory.to_string_lossy().as_ref());
    assert!(decoded.get("project").is_none());
    assert!(decoded.get("subpath").is_none());
}

#[tokio::test]
async fn update_renames_archives_and_records_each_change() {
    let start = temp_directory("session-update-start");
    let (app, state) = session_app(&start);
    let id = session_id(&create(&app, "{}").await);
    let uri = format!("/api/session/{id}");
    let mut events = state
        .bus
        .subscribe(&["session.renamed", "session.archived", "session.unarchived", "session.metadata.updated"], None);

    let response = app.handle(json_request("PATCH", &uri, &json!({ "title": "renamed" }).to_string())).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(text(response).await.is_empty());
    let event = events.try_recv().expect("a session.renamed event");
    assert_eq!(event.r#type(), "session.renamed");
    assert_eq!(event.data()["title"], "renamed");
    assert_eq!(get(&app, &id).await["data"]["title"], "renamed");
    assert!(events.try_recv().is_none());

    let response = app.handle(json_request("PATCH", &uri, &json!({ "archived": true }).to_string())).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let event = events.try_recv().expect("a session.archived event");
    assert_eq!(event.r#type(), "session.archived");
    assert!(get(&app, &id).await["data"]["time"]["archived"].as_f64().is_some());
    assert!(events.try_recv().is_none());

    let response = app.handle(json_request("PATCH", &uri, &json!({ "archived": true }).to_string())).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(events.try_recv().is_none());

    let response = app.handle(json_request("PATCH", &uri, &json!({ "archived": false }).to_string())).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let event = events.try_recv().expect("a session.unarchived event");
    assert_eq!(event.r#type(), "session.unarchived");
    assert!(get(&app, &id).await["data"]["time"]["archived"].is_null());
    assert!(events.try_recv().is_none());

    let response = app.handle(json_request("PATCH", &uri, &json!({ "metadata": { "k": "v" } }).to_string())).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let event = events.try_recv().expect("a session.metadata.updated event");
    assert_eq!(event.r#type(), "session.metadata.updated");
    assert_eq!(event.data()["metadata"], json!({ "k": "v" }));
    assert_eq!(get(&app, &id).await["data"]["metadata"], json!({ "k": "v" }));
    assert!(events.try_recv().is_none());

    let response = app.handle(json_request("PATCH", &uri, &json!({ "title": "" }).to_string())).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(events.try_recv().is_none());
}

#[tokio::test]
async fn remove_deletes_the_parent_and_its_children() {
    let start = temp_directory("session-remove-start");
    let (app, state) = session_app(&start);
    let parent_id = session_id(&create(&app, "{}").await);
    let child_id = session_id(&create(&app, &json!({ "parentID": parent_id }).to_string()).await);
    let mut events = state.bus.subscribe(&["session.deleted"], None);

    let response = app.handle(json_request("DELETE", &format!("/api/session/{parent_id}"), "")).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(text(response).await.is_empty());

    for expected in [&parent_id, &child_id] {
        let event = events.try_recv().expect("a session.deleted event");
        assert_eq!(event.r#type(), "session.deleted");
        assert_eq!(event.data()["sessionID"], expected.as_str());
    }
    assert!(events.try_recv().is_none());

    for id in [&parent_id, &child_id] {
        let response = app.handle(json_request("GET", &format!("/api/session/{id}"), "")).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            json_body(response).await,
            json!({
                "_tag": "SessionNotFoundError",
                "sessionID": id,
                "message": format!("Session not found: {id}"),
            })
        );
    }
}

#[tokio::test]
async fn missing_sessions_are_404() {
    let start = temp_directory("session-missing-start");
    let (app, _) = session_app(&start);
    let missing = json!({
        "_tag": "SessionNotFoundError",
        "sessionID": "ses_missing",
        "message": "Session not found: ses_missing",
    });

    for (method, payload) in [("GET", ""), ("DELETE", ""), ("PATCH", r#"{"title":"x"}"#)] {
        let response = app.handle(json_request(method, "/api/session/ses_missing", payload)).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(json_body(response).await, missing);
    }

    let response = app.handle(json_request("POST", "/api/session", r#"{"parentID":"ses_missing"}"#)).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(json_body(response).await, missing);
}

#[tokio::test]
async fn location_resolves_from_the_session_row_without_a_location_header() {
    let start = temp_directory("session-location-start");
    let other = temp_directory("session-location-other");
    let (app, _) = session_app(&start);
    let id =
        session_id(&create(&app, &json!({ "location": { "directory": other.to_string_lossy() } }).to_string()).await);

    let raw = RawRequest {
        path_params: vec![("sessionID".to_owned(), id)],
        query: String::new(),
        headers: Vec::new(),
        body: Vec::new(),
    };
    let Ok(lease) = app.location(&raw).await else {
        panic!("a location lease");
    };
    assert_eq!(lease.info.directory, other.to_string_lossy().as_ref());

    let raw = RawRequest {
        path_params: vec![("sessionID".to_owned(), "ses_missing".to_owned())],
        query: String::new(),
        headers: Vec::new(),
        body: Vec::new(),
    };
    let Err(response) = app.location(&raw).await else {
        panic!("a not-found response");
    };
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        json_body(response).await,
        json!({
            "_tag": "SessionNotFoundError",
            "sessionID": "ses_missing",
            "message": "Session not found: ses_missing",
        })
    );
}

#[tokio::test]
async fn prompt_admits_a_user_item_and_is_idempotent() {
    let start = temp_directory("session-prompt-start");
    let (app, state) = session_app(&start);
    let session = session_id(&create(&app, "{}").await);
    let mut events = state.bus.subscribe(&["session.inbox.enqueued"], None);

    let body = prompt(&app, &session, json!({ "text": "say hi" })).await;
    let data = &body["data"];
    let id = data["id"].as_str().expect("a message id").to_owned();
    assert!(id.starts_with("msg_"));
    assert_eq!(data["type"], "user");
    assert_eq!(data["sessionID"], session.as_str());
    assert_eq!(data["delivery"], "steer");
    assert_eq!(data["payload"]["text"], "say hi");
    assert!(data["time"]["created"].is_number());

    let event = events.try_recv().expect("a session.inbox.enqueued event");
    assert_eq!(event.r#type(), "session.inbox.enqueued");
    assert_eq!(event.data()["sessionID"], session.as_str());
    assert_eq!(event.data()["inboxID"], id.as_str());
    assert!(events.try_recv().is_none());

    let items = inbox(&app, &session).await;
    let items = items["data"].as_array().expect("an inbox list");
    assert_eq!(items.len(), 1);
    assert_eq!(&items[0], data);

    let repeated = prompt(&app, &session, json!({ "id": id, "text": "different" })).await;
    assert_eq!(&repeated["data"], data);
    assert!(events.try_recv().is_none());
    assert_eq!(inbox(&app, &session).await["data"].as_array().expect("an inbox list").len(), 1);
}

#[tokio::test]
async fn prompt_conflicts_when_the_message_id_is_reused() {
    let start = temp_directory("session-prompt-conflict-start");
    let (app, state) = session_app(&start);
    let first = session_id(&create(&app, "{}").await);
    let second = session_id(&create(&app, "{}").await);

    let body = prompt(&app, &first, json!({ "id": "msg_conflict", "text": "one" })).await;
    assert_eq!(body["data"]["payload"]["text"], "one");

    let response = app
        .handle(json_request(
            "POST",
            &format!("/api/session/{second}/prompt"),
            &json!({ "id": "msg_conflict", "text": "two" }).to_string(),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_body(response).await,
        json!({
            "_tag": "ConflictError",
            "message": "Prompt message ID conflicts with an existing durable record: msg_conflict",
            "resource": "msg_conflict",
        })
    );

    deliver(&state, &first, "msg_conflict");
    let repeated = prompt(&app, &first, json!({ "id": "msg_conflict", "text": "again" })).await;
    assert_eq!(repeated["data"]["payload"]["text"], "one");
    assert_eq!(repeated["data"]["delivery"], "steer");

    let response = app
        .handle(json_request(
            "POST",
            &format!("/api/session/{second}/prompt"),
            &json!({ "id": "msg_conflict", "text": "two" }).to_string(),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);

    let missing = json!({
        "_tag": "SessionNotFoundError",
        "sessionID": "ses_missing",
        "message": "Session not found: ses_missing",
    });
    let response =
        app.handle(json_request("POST", "/api/session/ses_missing/prompt", &json!({ "text": "x" }).to_string())).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(json_body(response).await, missing);
    let response = app.handle(json_request("GET", "/api/session/ses_missing/inbox", "")).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(json_body(response).await, missing);
}

#[tokio::test]
async fn prompt_with_resume_false_still_admits() {
    let start = temp_directory("session-prompt-resume-start");
    let (app, state) = session_app(&start);
    let session = session_id(&create(&app, "{}").await);
    let mut events = state.bus.subscribe(&["session.inbox.enqueued"], None);

    let body = prompt(&app, &session, json!({ "text": "queued", "resume": false })).await;
    let id = body["data"]["id"].as_str().expect("a message id");
    assert_eq!(body["data"]["payload"]["text"], "queued");
    let event = events.try_recv().expect("a session.inbox.enqueued event");
    assert_eq!(event.data()["inboxID"], id);
    assert!(events.try_recv().is_none());

    let items = inbox(&app, &session).await;
    let items = items["data"].as_array().expect("an inbox list");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], id);
}

#[tokio::test]
async fn prompt_rejects_files_and_skills() {
    let start = temp_directory("session-prompt-attachments-start");
    let (app, _) = session_app(&start);
    let session = session_id(&create(&app, "{}").await);
    let unsupported = json!({
        "_tag": "UnknownError",
        "message": "opencode-server does not implement prompt attachments yet",
    });
    for payload in [
        json!({ "text": "x", "files": [{ "uri": "file:///tmp/x" }] }),
        json!({ "text": "x", "skills": [{ "id": "some-skill" }] }),
    ] {
        let response =
            app.handle(json_request("POST", &format!("/api/session/{session}/prompt"), &payload.to_string())).await;
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        assert_eq!(json_body(response).await, unsupported);
    }
    assert_eq!(inbox(&app, &session).await["data"], json!([]));
}

#[tokio::test]
async fn messages_list_pages_filters_and_rejects_bad_cursors() {
    let start = temp_directory("session-messages-start");
    let (app, state) = session_app(&start);
    let session = session_id(&create(&app, "{}").await);
    for (id, text) in [("msg_a", "a"), ("msg_b", "b"), ("msg_c", "c")] {
        prompt(&app, &session, json!({ "id": id, "text": text })).await;
    }
    for id in ["msg_a", "msg_b", "msg_c"] {
        deliver(&state, &session, id);
    }

    let all = messages(&app, &session, "").await;
    assert_eq!(message_ids(&all), vec!["msg_c", "msg_b", "msg_a"]);
    assert!(all["cursor"]["previous"].is_string());
    let first_next = all["cursor"]["next"].as_str().expect("a next cursor").to_owned();
    assert_eq!(cursor_json(&first_next), json!({ "id": "msg_a", "order": "desc", "direction": "next" }));

    let page = messages(&app, &session, "?limit=2").await;
    assert_eq!(message_ids(&page), vec!["msg_c", "msg_b"]);
    let page_next = page["cursor"]["next"].as_str().expect("a next cursor").to_owned();
    assert_eq!(cursor_json(&page_next)["id"], "msg_b");

    let encoded = url::form_urlencoded::byte_serialize(page_next.as_bytes()).collect::<String>();
    let page = messages(&app, &session, &format!("?cursor={encoded}")).await;
    assert_eq!(message_ids(&page), vec!["msg_a"]);
    let previous = page["cursor"]["previous"].as_str().expect("a previous cursor").to_owned();
    assert_eq!(cursor_json(&previous), json!({ "id": "msg_a", "order": "desc", "direction": "previous" }));

    let page = messages(&app, &session, "?limit=2&order=asc").await;
    assert_eq!(message_ids(&page), vec!["msg_a", "msg_b"]);

    let page = messages(&app, &session, "?type=user").await;
    assert_eq!(message_ids(&page), vec!["msg_c", "msg_b", "msg_a"]);
    let page = messages(&app, &session, "?type=assistant").await;
    assert_eq!(page, json!({ "data": [], "cursor": {} }));

    let encoded = url::form_urlencoded::byte_serialize(first_next.as_bytes()).collect::<String>();
    let response = app
        .handle(json_request("GET", &format!("/api/session/{session}/message?cursor={encoded}&order=desc"), ""))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(response).await,
        json!({ "_tag": "InvalidCursorError", "message": "Cursor cannot be combined with order" })
    );

    let response =
        app.handle(json_request("GET", &format!("/api/session/{session}/message?cursor=not-a-cursor"), "")).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json_body(response).await, json!({ "_tag": "InvalidCursorError", "message": "Invalid cursor" }));

    let response = app.handle(json_request("GET", "/api/session/ses_missing/message", "")).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        json_body(response).await,
        json!({
            "_tag": "SessionNotFoundError",
            "sessionID": "ses_missing",
            "message": "Session not found: ses_missing",
        })
    );
}
