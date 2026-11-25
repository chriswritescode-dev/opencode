use std::path::Path;

use http::StatusCode;
use serde_json::Value;

use crate::{authorized, ready_app_with_services, services_at, temp_directory, text};

fn write_config(directory: &Path) {
    std::fs::write(
        directory.join("opencode.json"),
        r#"{"providers":{"custom":{"name":"Configured Custom Provider","package":"aisdk:@ai-sdk/openai-compatible","settings":{"apiKey":"secret"},"models":{"chat":{}}}}}"#,
    )
    .expect("write the project config");
}

async fn body_json(response: opencode_http::response::Response) -> Value {
    serde_json::from_str(&text(response).await).expect("a JSON body")
}

#[tokio::test]
async fn list_returns_the_configured_provider() {
    let project = temp_directory("provider-list");
    write_config(&project);
    let app = ready_app_with_services(services_at(&project));

    let response = app.handle(authorized("GET", "/api/provider")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["location"]["directory"], project.to_string_lossy().as_ref());
    let data = body["data"].as_array().expect("a provider list");
    let provider = data.iter().find(|provider| provider["id"] == "custom").expect("the configured provider");
    assert_eq!(provider["name"], "Configured Custom Provider");
    assert_eq!(provider["package"], "@opencode/ai/providers/openai-compatible");
    assert_eq!(provider["activation"], "enabled");
}

#[tokio::test]
async fn get_returns_the_provider() {
    let project = temp_directory("provider-get");
    write_config(&project);
    let app = ready_app_with_services(services_at(&project));

    let response = app.handle(authorized("GET", "/api/provider/custom")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["data"]["id"], "custom");
    assert_eq!(body["data"]["name"], "Configured Custom Provider");
    assert_eq!(body["data"]["package"], "@opencode/ai/providers/openai-compatible");
}

#[tokio::test]
async fn missing_provider_is_a_404() {
    let project = temp_directory("provider-missing");
    write_config(&project);
    let app = ready_app_with_services(services_at(&project));

    let response = app.handle(authorized("GET", "/api/provider/unknown")).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = body_json(response).await;
    assert_eq!(body["_tag"], "ProviderNotFoundError");
    assert_eq!(body["providerID"], "unknown");
    assert_eq!(body["message"], "Provider not found: unknown");
}

#[tokio::test]
async fn invalid_query_is_a_400() {
    let project = temp_directory("provider-invalid");
    write_config(&project);
    let app = ready_app_with_services(services_at(&project));

    for uri in ["/api/provider?location=1", "/api/provider/custom?location=1"] {
        let response = app.handle(authorized("GET", uri)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
        let body = body_json(response).await;
        assert_eq!(body["_tag"], "InvalidRequestError", "{uri}");
        assert_eq!(body["kind"], "Query", "{uri}");
    }
}
