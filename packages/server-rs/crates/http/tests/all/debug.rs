use std::path::Path;

use http::StatusCode;
use serde_json::Value;

use crate::{authorized, ready_app_with_services, services_at, temp_directory, text};

fn heap_uri(directory: &Path) -> String {
    let encoded = url::form_urlencoded::byte_serialize(directory.to_string_lossy().as_bytes()).collect::<String>();
    format!("/api/debug/heap-dump?directory={encoded}")
}

async fn body_json(response: opencode_http::response::Response) -> Value {
    serde_json::from_str(&text(response).await).expect("a JSON body")
}

#[tokio::test]
async fn heap_dump_rejects_a_relative_directory() {
    let start = temp_directory("heap-relative");
    let app = ready_app_with_services(services_at(&start));

    let response = app.handle(authorized("POST", "/api/debug/heap-dump?directory=capture")).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(body["_tag"], "InvalidRequestError");
    assert_eq!(body["message"], "directory must be an absolute path");
    assert_eq!(body["field"], "directory");
}

#[tokio::test]
async fn heap_dump_writes_a_private_memory_report() {
    let start = temp_directory("heap-dump");
    let services = services_at(&start);
    let app = ready_app_with_services(services.clone());
    let directory = start.join("capture").join("nested");

    let response = app.handle(authorized("POST", &heap_uri(&directory))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let path = Path::new(body["path"].as_str().expect("a path"));
    assert_eq!(path.parent(), Some(directory.as_path()));
    assert!(
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(&format!("heap-{}-", std::process::id())))
    );
    assert_eq!(body["pid"], f64::from(std::process::id()));
    assert!(body["memory"]["rss"].as_f64().is_some_and(|rss| rss >= 0.0));
    assert_eq!(body["memory"]["heapTotal"], 0);
    assert_eq!(body["memory"]["heapUsed"], 0);
    assert_eq!(body["memory"]["external"], 0);
    assert_eq!(body["memory"]["arrayBuffers"], 0);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path).expect("the report").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    let report: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read the report")).expect("a JSON report");
    assert_eq!(report["pid"], std::process::id());
    assert!(report["rss"].is_number());
    assert!(report["status"].is_object());
}

#[tokio::test]
async fn heap_dump_defaults_to_the_log_directory() {
    let start = temp_directory("heap-log");
    let services = services_at(&start);
    let app = ready_app_with_services(services.clone());

    let response = app.handle(authorized("POST", "/api/debug/heap-dump")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let path = Path::new(body["path"].as_str().expect("a path"));
    assert_eq!(path.parent(), Some(services.paths.log.as_path()));
}
