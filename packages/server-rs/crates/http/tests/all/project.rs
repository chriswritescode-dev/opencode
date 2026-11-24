use std::path::Path;
use std::sync::Arc;

use bytes::Bytes;
use http::{Request, StatusCode};
use opencode_http::app::App;
use serde_json::{Value, json};

use crate::{authorized, authorized_with, basic, ready_app_with_services, services_at, temp_directory, text};

fn directory_uri(directory: &Path) -> String {
    let encoded = url::form_urlencoded::byte_serialize(directory.to_string_lossy().as_bytes()).collect::<String>();
    format!("/api/location?location%5Bdirectory%5D={encoded}")
}

fn patch(uri: &str, body: &str, headers: &[(&str, &str)]) -> Request<Bytes> {
    let credential = basic("opencode", "pw");
    let mut builder = Request::builder()
        .method("PATCH")
        .uri(uri)
        .header("authorization", credential)
        .header("content-type", "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder.body(Bytes::from(body.to_owned())).expect("a request")
}

async fn body_json(response: opencode_http::response::Response) -> Value {
    serde_json::from_str(&text(response).await).expect("a JSON body")
}

async fn boot(app: &Arc<App>, directory: &Path) -> String {
    let response = app.handle(authorized("GET", &directory_uri(directory))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    body["project"]["id"].as_str().expect("a project id").to_owned()
}

fn project_ids(body: &Value) -> Vec<String> {
    body.as_array()
        .expect("an array of projects")
        .iter()
        .map(|project| project["id"].as_str().expect("a project id").to_owned())
        .collect()
}

fn find_project<'a>(body: &'a Value, id: &str) -> &'a Value {
    body.as_array()
        .expect("an array of projects")
        .iter()
        .find(|project| project["id"] == id)
        .unwrap_or_else(|| panic!("project {id} is missing"))
}

#[tokio::test]
async fn list_returns_both_booted_projects() {
    let start = temp_directory("project-list-start");
    let first = temp_directory("project-list-first");
    let second = temp_directory("project-list-second");
    let app = ready_app_with_services(services_at(&start));
    let first_id = boot(&app, &first).await;
    let second_id = boot(&app, &second).await;

    let response = app.handle(authorized("GET", "/api/project")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body.as_array().expect("an array of projects").len(), 2);

    for (directory, id) in [(&first, &first_id), (&second, &second_id)] {
        let project = find_project(&body, id);
        assert_eq!(project["canonical"], directory.to_string_lossy().as_ref());
        assert!(project["vcs"].is_null());
        assert!(project["name"].is_null());
        assert!(project["icon"].is_null());
        assert!(project["commands"].is_null());
        assert_eq!(project["sandboxes"], json!([]));
        assert!(project["time"].is_object());
        assert!(project["time"]["created"].as_f64().is_some());
        assert!(project["time"]["updated"].as_f64().is_some());
        assert!(project["time"]["active"].as_f64().is_some());
        assert!(project["time"]["archived"].is_null());
    }
}

#[tokio::test]
async fn list_excludes_archived_projects_unless_requested() {
    let start = temp_directory("project-archived-start");
    let active = temp_directory("project-archived-active");
    let archived = temp_directory("project-archived-old");
    let services = services_at(&start);
    let app = ready_app_with_services(services.clone());
    let active_id = boot(&app, &active).await;
    let archived_id = boot(&app, &archived).await;
    services
        .database
        .read(|connection| opencode_storage::project::set_archived(connection, &archived_id, Some(1)))
        .expect("archive the project");

    let default = body_json(app.handle(authorized("GET", "/api/project")).await).await;
    assert_eq!(project_ids(&default), vec![active_id.clone()]);

    let hidden = body_json(app.handle(authorized("GET", "/api/project?archived=false")).await).await;
    assert_eq!(project_ids(&hidden), vec![active_id.clone()]);

    let included = body_json(app.handle(authorized("GET", "/api/project?archived=true")).await).await;
    let ids = project_ids(&included);
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&active_id));
    assert!(ids.contains(&archived_id));
    assert_eq!(find_project(&included, &archived_id)["time"]["archived"], 1);
}

#[tokio::test]
async fn update_writes_the_name_icon_and_commands() {
    let start = temp_directory("project-update-start");
    let directory = temp_directory("project-update-target");
    let app = ready_app_with_services(services_at(&start));
    let id = boot(&app, &directory).await;

    let payload = json!({
        "name": "Renamed",
        "icon": { "override": "https://example.test/icon.svg", "color": "#123456" },
        "commands": { "start": "bun dev" },
    });
    let response = app.handle(patch(&format!("/api/project/{id}"), &payload.to_string(), &[])).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["id"], id.as_str());
    assert_eq!(body["canonical"], directory.to_string_lossy().as_ref());
    assert_eq!(body["name"], "Renamed");
    assert_eq!(body["icon"]["override"], "https://example.test/icon.svg");
    assert_eq!(body["icon"]["color"], "#123456");
    assert_eq!(body["commands"]["start"], "bun dev");

    let listed = body_json(app.handle(authorized("GET", "/api/project")).await).await;
    let project = find_project(&listed, &id);
    assert_eq!(project["name"], "Renamed");
    assert_eq!(project["icon"]["override"], "https://example.test/icon.svg");
    assert_eq!(project["icon"]["color"], "#123456");
    assert_eq!(project["commands"]["start"], "bun dev");
}

#[tokio::test]
async fn update_clears_metadata_with_empty_strings() {
    let start = temp_directory("project-clear-start");
    let directory = temp_directory("project-clear-target");
    let app = ready_app_with_services(services_at(&start));
    let id = boot(&app, &directory).await;

    let set = json!({
        "name": "Temporary",
        "icon": { "override": "https://example.test/icon.svg", "color": "#123456" },
        "commands": { "start": "bun dev" },
    });
    let response = app.handle(patch(&format!("/api/project/{id}"), &set.to_string(), &[])).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["name"], "Temporary");

    let clear = json!({ "name": "", "icon": { "override": "", "color": "" }, "commands": { "start": "" } });
    let response = app.handle(patch(&format!("/api/project/{id}"), &clear.to_string(), &[])).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert!(body["name"].is_null());
    assert!(body["icon"].is_null());
    assert!(body["commands"].is_null());

    let listed = body_json(app.handle(authorized("GET", "/api/project")).await).await;
    let project = find_project(&listed, &id);
    assert!(project["name"].is_null());
    assert!(project["icon"].is_null());
    assert!(project["commands"].is_null());
}

#[tokio::test]
async fn updating_an_unknown_project_is_404() {
    let start = temp_directory("project-update-missing");
    let app = ready_app_with_services(services_at(&start));

    let response = app.handle(patch("/api/project/unknown", "{}", &[])).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = body_json(response).await;
    assert_eq!(
        body,
        json!({
            "_tag": "ProjectNotFoundError",
            "projectID": "unknown",
            "message": "Project not found: unknown",
        })
    );
}

#[tokio::test]
async fn list_rejects_an_invalid_archived_query() {
    let start = temp_directory("project-invalid-query");
    let app = ready_app_with_services(services_at(&start));

    let response = app.handle(authorized("GET", "/api/project?archived=maybe")).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(body["_tag"], "InvalidRequestError");
    assert_eq!(body["kind"], "Query");
}

#[tokio::test]
async fn unrelated_directory_header_does_not_boot_a_location() {
    let start = temp_directory("project-header-start");
    let directory = temp_directory("project-header-target");
    let services = services_at(&start);
    let app = ready_app_with_services(services.clone());
    let id = boot(&app, &directory).await;

    let response = app
        .handle(authorized_with("GET", "/api/project", &[("x-opencode-directory", "/unrelated-missing-directory")]))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(project_ids(&body), vec![id.clone()]);

    let headers = [("x-opencode-directory", "/unrelated-missing-directory")];
    let response =
        app.handle(patch(&format!("/api/project/{id}"), &json!({ "name": "From header" }).to_string(), &headers)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["name"], "From header");

    let response = app.handle(authorized("GET", "/api/debug/location")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body, json!([{ "directory": directory.to_string_lossy() }]));
}
