use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use http::Request;
use opencode_bus::{Definition, PublishOptions};
use opencode_config::paths::GlobalPaths;
use opencode_http::app::{App, Options};
use opencode_http::services::GlobalServices;
use opencode_http::state::ServerState;
use opencode_storage::DatabaseLocation;
use serde_json::{Map, Value, json};

use crate::{basic, temp_directory};

const CAPTURE: &str = include_str!("../fixtures/permission-ask-reply.json");
const SESSION_ID: &str = "ses_00000000000000000000000001";
const TIME_WINDOW: f64 = 3_600_000.0;

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

async fn boot_and_seed(services: &GlobalServices, state: &ServerState, directory: &Path) {
    let reference = opencode_location::location::LocationRef::new(directory.to_string_lossy().into_owned(), None);
    let lease = services.locations.get(&reference).await.expect("boot the location");
    let project_id = lease.services().info.project.id.clone();
    let definition = Definition::latest_durable("session.created").expect("session.created is durable");
    state
        .bus
        .publish(
            &definition,
            json!({
                "sessionID": SESSION_ID,
                "projectID": project_id,
                "location": { "directory": directory.to_string_lossy() },
                "slug": SESSION_ID,
                "version": "1",
            }),
            PublishOptions { id: Some(format!("evt_{SESSION_ID}")), ..Default::default() },
        )
        .expect("publish session.created");
}

fn json_request(method: &str, uri: &str, directory: &Path, body: Option<&Value>) -> Request<Bytes> {
    let credential = basic("opencode", "pw");
    let encoded = directory.to_string_lossy();
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", credential)
        .header("content-type", "application/json")
        .header("x-opencode-directory", encoded.as_ref())
        .body(body.map_or_else(Bytes::new, |body| Bytes::from(body.to_string())))
        .expect("a request")
}

async fn capture(app: &App, request: Request<Bytes>) -> Value {
    use http_body_util::BodyExt;
    let response = app.handle(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body().collect().await.expect("a body").to_bytes();
    let body = if bytes.is_empty() {
        Value::String(String::new())
    } else {
        serde_json::from_slice(&bytes).expect("a JSON body")
    };
    json!({ "status": status, "body": body })
}

#[derive(Default)]
struct NormalizeState {
    ids: HashMap<String, usize>,
    counts: HashMap<String, usize>,
}

struct Normalize<'a> {
    directory: &'a str,
    start: f64,
    end: f64,
}

fn normalize(value: &Value, context: &Normalize<'_>, state: &mut NormalizeState, key: Option<&str>) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(|item| normalize(item, context, state, None)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), normalize(value, context, state, Some(key))))
                .collect::<Map<String, Value>>(),
        ),
        Value::String(text) => Value::String(normalize_string(text, context, state, key)),
        Value::Number(number) => match number.as_f64() {
            Some(value) if value >= context.start - TIME_WINDOW && value <= context.end + TIME_WINDOW => {
                Value::String("<time>".to_owned())
            }
            _ => value.clone(),
        },
        other => other.clone(),
    }
}

fn normalize_string(text: &str, context: &Normalize<'_>, state: &mut NormalizeState, key: Option<&str>) -> String {
    if key == Some("projectID") {
        return "<project>".to_owned();
    }
    let text =
        if context.directory.is_empty() { text.to_owned() } else { text.replace(context.directory, "<root>/work") };
    replace_ids(&text, state)
}

fn replace_ids(text: &str, state: &mut NormalizeState) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (start, character) in text.char_indices() {
        if start < last || !character.is_ascii_lowercase() {
            continue;
        }
        let Some((prefix, end)) = id_at(text, start) else { continue };
        out.push_str(&text[last..start]);
        let next = match state.ids.get(&text[start..end]) {
            Some(index) => *index,
            None => {
                let next = state.counts.get(prefix).copied().unwrap_or(0) + 1;
                state.counts.insert(prefix.to_owned(), next);
                state.ids.insert(text[start..end].to_owned(), next);
                next
            }
        };
        out.push_str(&format!("<{prefix}:{next}>"));
        last = end;
    }
    out.push_str(&text[last..]);
    out
}

fn id_at(text: &str, start: usize) -> Option<(&str, usize)> {
    let bytes = text.as_bytes();
    if start > 0 && is_word(bytes[start - 1]) {
        return None;
    }
    let mut prefix_end = start;
    while prefix_end < bytes.len() && bytes[prefix_end].is_ascii_lowercase() {
        prefix_end += 1;
    }
    if !(2..=5).contains(&(prefix_end - start)) || bytes.get(prefix_end) != Some(&b'_') {
        return None;
    }
    let end = prefix_end + 27;
    if end > bytes.len() || !bytes[prefix_end + 1..end].iter().all(u8::is_ascii_alphanumeric) {
        return None;
    }
    if end < bytes.len() && is_word(bytes[end]) {
        return None;
    }
    Some((&text[start..prefix_end], end))
}

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn normalize_capture(value: &Value, context: &Normalize<'_>) -> Value {
    normalize(value, context, &mut NormalizeState::default(), None)
}

fn captured_step(step: &Value) -> Value {
    json!({ "status": step["status"], "body": step["body"] })
}

fn now_millis() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).expect("the clock is after the epoch").as_millis() as f64
}

#[tokio::test]
async fn permission_ask_reply_matches_the_typescript_capture() {
    let reference: Value = serde_json::from_str(CAPTURE).expect("the recorded capture");
    let expected = reference["steps"].as_array().expect("captured steps");
    assert_eq!(expected.len(), 11, "the capture records the session creation and ten further steps");

    let start = temp_directory("permission-oracle-start");
    let home = temp_directory("permission-oracle-home");
    std::fs::write(
        start.join("opencode.json"),
        br#"{"permissions":[{"action":"bash","resource":"*","effect":"ask"}]}"#,
    )
    .expect("write the project config");
    let state = state();
    let services = services(&state, &start, &home);
    let app = app(&state, services.clone());
    boot_and_seed(&services, &state, &start).await;

    let session = format!("/api/session/{SESSION_ID}/permission");
    let started = now_millis();
    let create = json!({ "action": "bash", "resources": ["ls"], "save": ["ls *"] });
    let created = capture(&app, json_request("POST", &session, &start, Some(&create))).await;
    let request_id = created["body"]["data"]["id"].as_str().expect("a permission id").to_owned();
    let mut steps = vec![created];

    steps.push(capture(&app, json_request("GET", &session, &start, None)).await);
    steps.push(capture(&app, json_request("GET", "/api/permission/request", &start, None)).await);
    steps.push(capture(&app, json_request("GET", &format!("{session}/{request_id}"), &start, None)).await);
    steps.push(
        capture(
            &app,
            json_request(
                "POST",
                &format!("{session}/{request_id}/reply"),
                &start,
                Some(&json!({ "decision": "always" })),
            ),
        )
        .await,
    );
    let saved = capture(&app, json_request("GET", "/api/permission/saved", &start, None)).await;
    let saved_id = saved["body"]["data"][0]["id"].as_str().expect("a saved permission id").to_owned();
    steps.push(saved);
    steps.push(
        capture(
            &app,
            json_request("POST", &session, &start, Some(&json!({ "action": "bash", "resources": ["ls -la"] }))),
        )
        .await,
    );
    steps.push(
        capture(
            &app,
            json_request("POST", &format!("{session}/per_unknown/reply"), &start, Some(&json!({ "decision": "once" }))),
        )
        .await,
    );
    steps.push(
        capture(
            &app,
            json_request(
                "PATCH",
                &format!("/api/permission/saved/{saved_id}"),
                &start,
                Some(&json!({ "resource": "ls -lh" })),
            ),
        )
        .await,
    );
    steps.push(capture(&app, json_request("DELETE", &format!("/api/permission/saved/{saved_id}"), &start, None)).await);

    let ended = now_millis();
    let directory = start.to_string_lossy();
    let context = Normalize { directory: directory.as_ref(), start: started, end: ended };
    let expected = normalize_capture(&Value::Array(expected.iter().skip(1).map(captured_step).collect()), &context);
    let actual = normalize_capture(&Value::Array(steps.iter().map(captured_step).collect()), &context);
    let expected = expected.as_array().expect("normalized steps");
    let actual = actual.as_array().expect("normalized steps");
    assert_eq!(expected.len(), actual.len(), "step count");
    for (index, (expected, actual)) in expected.iter().zip(actual.iter()).enumerate() {
        assert_eq!(actual, expected, "step {index}");
    }
}
