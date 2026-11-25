use std::path::Path;

use http::StatusCode;
use serde_json::Value;

use crate::{authorized, ready_app_with_services, services_at, services_at_with_env, temp_directory, text};

fn write_config(directory: &Path) {
    std::fs::write(
        directory.join("opencode.json"),
        r#"{"providers":{"custom":{"package":"aisdk:@ai-sdk/openai-compatible","settings":{"apiKey":"secret"},"models":{"chat":{}}}},"model":"custom/chat"}"#,
    )
    .expect("write the project config");
}

async fn body_json(response: opencode_http::response::Response) -> Value {
    serde_json::from_str(&text(response).await).expect("a JSON body")
}

#[tokio::test]
async fn list_filters_configured_models_by_type() {
    let project = temp_directory("model-list");
    write_config(&project);
    let app = ready_app_with_services(services_at(&project));

    let response = app.handle(authorized("GET", "/api/model")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["location"]["directory"], project.to_string_lossy().as_ref());
    let data = body["data"].as_array().expect("a model list");
    assert!(data.iter().any(|model| model["id"] == "chat" && model["providerID"] == "custom"));

    let response = app.handle(authorized("GET", "/api/model?types=image")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let data = body["data"].as_array().expect("a model list");
    assert!(!data.iter().any(|model| model["providerID"] == "custom"));

    let response = app.handle(authorized("GET", "/api/model?types=image&types=chat")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let data = body["data"].as_array().expect("a model list");
    assert!(data.iter().any(|model| model["id"] == "chat"));
}

#[tokio::test]
async fn default_returns_the_configured_model() {
    let project = temp_directory("model-default");
    write_config(&project);
    let app = ready_app_with_services(services_at(&project));

    let response = app.handle(authorized("GET", "/api/model/default")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["data"]["id"], "chat");
    assert_eq!(body["data"]["providerID"], "custom");
}

#[tokio::test]
async fn default_omits_data_without_any_model() {
    let project = temp_directory("model-default-none");
    std::fs::write(project.join("models.json"), "{}").expect("write the models catalog");
    let models = project.join("models.json").to_string_lossy().into_owned();
    let app = ready_app_with_services(services_at_with_env(&project, &[("OPENCODE_MODELS_PATH", models)]));

    let response = app.handle(authorized("GET", "/api/model/default")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let object = body.as_object().expect("a response object");
    assert!(object.contains_key("location"));
    assert!(!object.contains_key("data"));

    let response = app.handle(authorized("GET", "/api/model")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["data"], serde_json::json!([]));
}

#[tokio::test]
async fn invalid_query_is_a_400() {
    let project = temp_directory("model-invalid");
    let app = ready_app_with_services(services_at(&project));

    for uri in ["/api/model?location=1", "/api/model/default?location=1"] {
        let response = app.handle(authorized("GET", uri)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
        let body = body_json(response).await;
        assert_eq!(body["_tag"], "InvalidRequestError", "{uri}");
        assert_eq!(body["kind"], "Query", "{uri}");
    }
}
