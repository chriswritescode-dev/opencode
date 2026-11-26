use std::path::Path;

use http::StatusCode;
use serde_json::{Value, json};

use crate::{authorized, ready_app_with_services, services_at, temp_directory, text};

fn uri(directory: &Path) -> String {
    let encoded = url::form_urlencoded::byte_serialize(directory.to_string_lossy().as_bytes()).collect::<String>();
    format!("/api/reference?location%5Bdirectory%5D={encoded}")
}

async fn body_json(response: opencode_http::response::Response) -> Value {
    serde_json::from_str(&text(response).await).expect("a JSON body")
}

#[tokio::test]
async fn lists_local_references_in_config_order() {
    let start = temp_directory("reference-start");
    let project = temp_directory("reference-project");
    std::fs::write(
        project.join("opencode.json"),
        r#"{"references":{"docs":{"path":"docs","description":"Docs","hidden":true},"shared":"./shared","local":"~/notes","invalid":"not-a-repo","invalid-branch":{"repository":"owner/repo","branch":"../escape"},"file":"file:///docs"}}"#,
    )
    .expect("write the project config");

    let services = services_at(&start);
    let home = services.paths.home.clone();
    let app = ready_app_with_services(services);

    let response = app.handle(authorized("GET", &uri(&project))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;

    assert_eq!(body["location"]["directory"], project.to_string_lossy().as_ref());

    let docs = project.join("docs").to_string_lossy().into_owned();
    let shared = project.join("shared").to_string_lossy().into_owned();
    let notes = home.join("notes").to_string_lossy().into_owned();
    assert_eq!(
        body["data"],
        json!([
            {
                "name": "docs",
                "path": docs,
                "description": "Docs",
                "hidden": true,
                "source": { "type": "local", "path": docs },
            },
            { "name": "shared", "path": shared, "source": { "type": "local", "path": shared } },
            { "name": "local", "path": notes, "source": { "type": "local", "path": notes } },
        ])
    );

    for entry in body["data"].as_array().expect("an array of references") {
        let source = entry["source"].as_object().expect("a source object");
        assert!(!source.contains_key("description"));
        assert!(!source.contains_key("hidden"));
    }
}

#[tokio::test]
async fn lists_references_of_a_missing_location_as_not_found() {
    let start = temp_directory("reference-missing-start");
    let app = ready_app_with_services(services_at(&start));

    let response = app.handle(authorized("GET", &uri(&start.join("absent")))).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(response).await["_tag"], "LocationNotFoundError");
}
