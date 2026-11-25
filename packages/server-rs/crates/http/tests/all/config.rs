use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::sync::Arc;

use bytes::Bytes;
use http::{Request, StatusCode};
use opencode_config::paths::GlobalPaths;
use opencode_http::services::GlobalServices;
use opencode_storage::{Database, DatabaseLocation};
use serde_json::Value;
use tokio::time::{Duration, Instant, sleep};

use crate::{authorized, basic, ready_app_with_services, services_at, temp_directory, text};

fn config_uri(directory: &Path) -> String {
    let encoded = url::form_urlencoded::byte_serialize(directory.to_string_lossy().as_bytes()).collect::<String>();
    format!("/api/config?location%5Bdirectory%5D={encoded}")
}

fn patch_json(uri: &str, directory: &Path, body: &str) -> Request<Bytes> {
    let credential = basic("opencode", "pw");
    let directory = directory.to_string_lossy();
    Request::builder()
        .method("PATCH")
        .uri(uri)
        .header("authorization", credential)
        .header("content-type", "application/json")
        .header("x-opencode-directory", directory.as_ref())
        .body(Bytes::from(body.to_owned()))
        .expect("a request")
}

async fn body_json(response: opencode_http::response::Response) -> Value {
    serde_json::from_str(&text(response).await).expect("a JSON body")
}

fn entry_for<'a>(body: &'a Value, path: &Path) -> Option<&'a Value> {
    let target = path.to_string_lossy();
    body.as_array()?.iter().find(|entry| entry["path"].as_str() == Some(target.as_ref()))
}

fn write_project_config(directory: &Path, shell: &str) {
    std::fs::write(directory.join("opencode.json"), format!(r#"{{"shell":"{shell}"}}"#))
        .expect("write the project config");
}

#[tokio::test]
async fn get_returns_the_project_document() {
    let start = temp_directory("config-get-start");
    let project = temp_directory("config-get-project");
    write_project_config(&project, "/bin/zsh");
    let app = ready_app_with_services(services_at(&start));

    let response = app.handle(authorized("GET", &config_uri(&project))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let entry = entry_for(&body, &project.join("opencode.json")).expect("the project document");
    assert_eq!(entry["type"], "document");
    assert_eq!(entry["info"]["shell"], "/bin/zsh");
}

#[tokio::test]
async fn shells_uses_the_server_path() {
    let start = temp_directory("config-shells-start");
    let home = temp_directory("config-shells-home");
    let path = "/nonexistent-opencode-test-path";
    let (database, _) = Database::open(DatabaseLocation::Memory, &std::env::temp_dir()).expect("open the database");
    let database = Arc::new(database);
    let paths =
        GlobalPaths::from_vars(|name| (name == "HOME").then(|| home.clone().into_os_string())).expect("global paths");
    let env = move |name: &str| (name == "PATH").then(|| OsString::from(path));
    let bus = opencode_bus::Bus::new(database.clone(), opencode_bus::BusOptions::default());
    let services = Arc::new(GlobalServices::new(
        database,
        paths,
        start.to_string_lossy().into_owned(),
        Arc::new(env),
        bus,
        "test-version".to_owned(),
        "test".to_owned(),
    ));
    let app = ready_app_with_services(services.clone());

    let response = app.handle(authorized("GET", "/api/config/shell")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;

    let expected = opencode_config::handlers::config_shells_with(
        &services.paths.bin,
        None,
        Path::new("/etc/shells"),
        Some(OsStr::new(path)),
    );
    let expected =
        serde_json::from_str::<Value>(&opencode_contract::codec::encode_json(&expected)).expect("the expected shells");
    assert_eq!(body, expected);
    assert!(body.as_array().is_some_and(|shells| !shells.is_empty()));
}

#[tokio::test]
async fn update_writes_the_global_config_and_get_shows_it() {
    let start = temp_directory("config-update-start");
    let project = temp_directory("config-update-project");
    write_project_config(&project, "/bin/sh");
    let services = services_at(&start);
    let app = ready_app_with_services(services.clone());

    let response = app.handle(authorized("GET", &config_uri(&project))).await;
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await;

    let response = app.handle(patch_json("/api/experimental/config", &project, r#"{"shell":"/bin/zsh"}"#)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(text(response).await, "");

    let file = services.paths.config.join("opencode.jsonc");
    let written = std::fs::read_to_string(&file).expect("the global config document");
    assert!(written.contains("/bin/zsh"));

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let response = app.handle(authorized("GET", &config_uri(&project))).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        if entry_for(&body, &file).is_some_and(|entry| entry["info"]["shell"] == "/bin/zsh") {
            break;
        }
        assert!(Instant::now() < deadline, "the updated shell never appeared in the config");
        sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn missing_directory_is_a_location_not_found() {
    let start = temp_directory("config-missing-start");
    let missing = start.join("missing");
    let services = services_at(&start);
    let app = ready_app_with_services(services.clone());

    let response = app.handle(authorized("GET", &config_uri(&missing))).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = body_json(response).await;
    assert_eq!(body["_tag"], "LocationNotFoundError");
    assert_eq!(body["location"]["directory"], missing.to_string_lossy().as_ref());
    assert_eq!(body["message"], format!("Location not found: {}", missing.to_string_lossy()));

    assert!(services.projects.list(false).await.expect("list the projects").is_empty());
}

#[tokio::test]
async fn two_locations_return_their_own_project_entries() {
    let start = temp_directory("config-two-start");
    let first = temp_directory("config-two-first");
    let second = temp_directory("config-two-second");
    write_project_config(&first, "/bin/zsh");
    write_project_config(&second, "/bin/bash");
    let app = ready_app_with_services(services_at(&start));

    for (directory, shell, other) in [(&first, "/bin/zsh", &second), (&second, "/bin/bash", &first)] {
        let response = app.handle(authorized("GET", &config_uri(directory))).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        let entry = entry_for(&body, &directory.join("opencode.json")).expect("the project document");
        assert_eq!(entry["info"]["shell"], shell);
        assert!(entry_for(&body, &other.join("opencode.json")).is_none());
        assert!(!body.to_string().contains(other.join("opencode.json").to_string_lossy().as_ref()));
    }
}
