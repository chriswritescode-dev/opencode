use http::StatusCode;
use serde_json::Value;

use crate::{authorized, ready_app_with_services, services_with_config, temp_directory, text};

const PARITY_CONFIG: &str = r#"{"model":"openai-test/test-model","providers":{"openai-test":{"name":"OpenAI Test","package":"@ai-sdk/openai-compatible","settings":{"apiKey":"parity","baseURL":"http://127.0.0.1:9/v1"},"models":{"test-model":{"name":"Test Model","capabilities":{"tools":true,"input":["text"],"output":["text"]},"cost":{"input":0,"output":0},"limit":{"context":100000,"output":10000}}}},"anthropic-test":{"name":"Anthropic Test","package":"@ai-sdk/anthropic","settings":{"apiKey":"parity","baseURL":"http://127.0.0.1:9/v1"},"models":{"test-model":{"name":"Test Model","capabilities":{"tools":true,"input":["text"],"output":["text"]},"cost":{"input":0,"output":0},"limit":{"context":100000,"output":10000}}}}}}"#;

fn variant_ids(model: &Value) -> Vec<&str> {
    model["variants"]
        .as_array()
        .expect("a variant list")
        .iter()
        .map(|variant| variant["id"].as_str().expect("a variant id"))
        .collect()
}

#[tokio::test]
async fn model_list_returns_the_parity_catalog() {
    let start = temp_directory("model-list-start");
    let app = ready_app_with_services(services_with_config(&start, PARITY_CONFIG));
    let response = app.handle(authorized("GET", "/api/model")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("content-type").and_then(|value| value.to_str().ok()), Some("application/json"));
    let body: Value = serde_json::from_str(&text(response).await).expect("a JSON body");

    assert_eq!(body["location"]["directory"], start.to_string_lossy().as_ref());
    let data = body["data"].as_array().expect("a model list");
    assert_eq!(data.len(), 2);
    assert_eq!(data[0]["providerID"], "openai-test");
    assert_eq!(data[1]["providerID"], "anthropic-test");
    assert_eq!(data[0]["package"], "@opencode/ai/providers/openai-compatible");
    assert_eq!(data[1]["package"], "@opencode/ai/providers/anthropic");
    assert_eq!(data[0]["settings"]["baseURL"], "http://127.0.0.1:9/v1");
    assert_eq!(data[0]["settings"]["provider"], "openai-test");
    assert_eq!(data[1]["settings"]["baseURL"], "http://127.0.0.1:9/v1");
    assert_eq!(variant_ids(&data[0]), vec!["low", "medium", "high"]);
    assert_eq!(variant_ids(&data[1]), vec!["low", "medium", "high", "xhigh", "max"]);
}

#[tokio::test]
async fn model_list_filters_by_type() {
    let start = temp_directory("model-filter-start");
    let app = ready_app_with_services(services_with_config(&start, PARITY_CONFIG));
    let response = app.handle(authorized("GET", "/api/model?types=image")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_str(&text(response).await).expect("a JSON body");
    assert_eq!(body["data"], serde_json::json!([]));
}
