use std::path::Path;
use std::sync::Arc;

use http::StatusCode;
use opencode_location::location::LocationRef;
use serde_json::Value;

use crate::{authorized, authorized_with, ready_app_with_services, services_at, temp_directory, text};

fn directory_uri(directory: &Path) -> String {
    let encoded = url::form_urlencoded::byte_serialize(directory.to_string_lossy().as_bytes()).collect::<String>();
    format!("/api/location?location%5Bdirectory%5D={encoded}")
}

fn evict_uri(directory: &Path) -> String {
    let encoded = url::form_urlencoded::byte_serialize(directory.to_string_lossy().as_bytes()).collect::<String>();
    format!("/api/debug/location?location%5Bdirectory%5D={encoded}")
}

async fn body_json(response: opencode_http::response::Response) -> Value {
    serde_json::from_str(&text(response).await).expect("a JSON body")
}

#[tokio::test]
async fn location_get_resolves_the_header_query_and_start_directory() {
    let start = temp_directory("location-start");
    let header = temp_directory("location-header");
    let query = temp_directory("location-query");
    let app = ready_app_with_services(services_at(&start));

    let response = app.handle(authorized("GET", "/api/location")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["directory"], start.to_string_lossy().as_ref());

    let response = app
        .handle(authorized_with("GET", "/api/location", &[("x-opencode-directory", header.to_string_lossy().as_ref())]))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["directory"], header.to_string_lossy().as_ref());
    assert!(body["project"]["id"].as_str().is_some_and(|id| !id.is_empty()));

    let response = app.handle(authorized("GET", &directory_uri(&query))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["directory"], query.to_string_lossy().as_ref());
}

#[tokio::test]
async fn location_get_missing_directory_is_404_and_persists_no_project() {
    let start = temp_directory("location-missing-start");
    let missing = start.join("missing");
    let services = services_at(&start);
    let app = ready_app_with_services(services.clone());

    let response = app
        .handle(authorized_with(
            "GET",
            "/api/location",
            &[("x-opencode-directory", missing.to_string_lossy().as_ref())],
        ))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = body_json(response).await;
    assert_eq!(body["_tag"], "LocationNotFoundError");
    assert_eq!(body["location"]["directory"], missing.to_string_lossy().as_ref());

    let count = services
        .database
        .read(|connection| Ok(connection.query_row("SELECT COUNT(*) FROM project", [], |row| row.get::<_, i64>(0))?))
        .expect("count projects");
    assert_eq!(count, 0);
}

#[tokio::test]
async fn reload_rebuilds_the_loaded_locations() {
    let root = temp_directory("location-reload");
    let services = services_at(&root);
    let app = ready_app_with_services(services.clone());
    let reference = LocationRef::new(root.to_string_lossy().into_owned(), None);

    let lease = services.locations.get(&reference).await.expect("boot the location");
    let before = lease.services().clone();
    drop(lease);

    let response = app.handle(authorized("POST", "/api/location/reload")).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(text(response).await, "");

    let rebuilt = services.locations.services(&reference).await.expect("the reloaded location");
    assert!(!Arc::ptr_eq(&before, &rebuilt));
    let lease = services.locations.get(&reference).await.expect("boot after reload");
    assert!(Arc::ptr_eq(lease.services(), &rebuilt));

    let response = app.handle(authorized("GET", "/api/location")).await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn location_list_reports_loaded_locations_in_order() {
    let start = temp_directory("location-list-start");
    let first = temp_directory("location-list-first");
    let second = temp_directory("location-list-second");
    let app = ready_app_with_services(services_at(&start));

    for directory in [&first, &second] {
        let response = app.handle(authorized("GET", &directory_uri(directory))).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    let response = app.handle(authorized("GET", "/api/debug/location")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(
        body,
        serde_json::json!([
            { "directory": first.to_string_lossy() },
            { "directory": second.to_string_lossy() },
        ])
    );
}

#[tokio::test]
async fn evicting_an_unloaded_location_is_a_no_op() {
    let start = temp_directory("location-evict-start");
    let unloaded = temp_directory("location-evict-unloaded");
    let services = services_at(&start);
    let app = ready_app_with_services(services.clone());

    let response = app.handle(authorized("DELETE", &evict_uri(&unloaded))).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(services.locations.keys().await.is_empty());
}

#[tokio::test]
async fn evicting_a_leased_location_keeps_the_lease_and_reboots_the_next_get() {
    let root = temp_directory("location-evict-leased");
    let services = services_at(&root);
    let app = ready_app_with_services(services.clone());
    let reference = LocationRef::new(root.to_string_lossy().into_owned(), None);

    let held = services.locations.get(&reference).await.expect("boot the location");
    let held_services = held.services().clone();

    let response = app.handle(authorized("DELETE", &evict_uri(&root))).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    assert_eq!(held.services().info.directory, reference.canonical().directory);
    assert!(services.locations.services(&reference).await.is_none());

    let next = services.locations.get(&reference).await.expect("boot after eviction");
    assert!(!Arc::ptr_eq(next.services(), &held_services));

    let response = app.handle(authorized("GET", &directory_uri(&root))).await;
    assert_eq!(response.status(), StatusCode::OK);
}
