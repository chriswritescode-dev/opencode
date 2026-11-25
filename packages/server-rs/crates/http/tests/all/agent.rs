use http::StatusCode;
use serde_json::{Value, json};

use crate::{authorized, ready_app_with_services, services_with_config, temp_directory, text};

fn ids(body: &Value) -> Vec<&str> {
    body["data"]
        .as_array()
        .expect("an agent list")
        .iter()
        .map(|agent| agent["id"].as_str().expect("an agent id"))
        .collect()
}

fn assert_deny_tail(agent: &Value) {
    let permissions = agent["permissions"].as_array().expect("a ruleset");
    let tail = &permissions[permissions.len() - 2..];
    assert_eq!(tail[0], json!({ "action": "browser", "resource": "*", "effect": "deny" }));
    assert_eq!(tail[1], json!({ "action": "skill", "resource": "browser", "effect": "deny" }));
}

#[tokio::test]
async fn agent_list_returns_build_first() {
    let start = temp_directory("agent-list-start");
    let app = ready_app_with_services(services_with_config(&start, r#"{"agents":{"title":{"disabled":true}}}"#));
    let response = app.handle(authorized("GET", "/api/agent")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("content-type").and_then(|value| value.to_str().ok()), Some("application/json"));
    let body: Value = serde_json::from_str(&text(response).await).expect("a JSON body");

    assert_eq!(body["location"]["directory"], start.to_string_lossy().as_ref());
    assert_eq!(ids(&body), vec!["build", "general", "explore", "compaction", "summary", "plan"]);
    assert_eq!(body["data"][0]["mode"], "primary");
    assert_eq!(body["data"][0]["hidden"], false);
    for agent in body["data"].as_array().expect("an agent list") {
        assert_deny_tail(agent);
    }
}

#[tokio::test]
async fn agent_list_applies_config_agents() {
    let start = temp_directory("agent-config-start");
    let config =
        r#"{"agents":{"general":{"disabled":true},"reviewer":{"description":"Reviewer","mode":"primary","steps":2}}}"#;
    let app = ready_app_with_services(services_with_config(&start, config));
    let response = app.handle(authorized("GET", "/api/agent")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_str(&text(response).await).expect("a JSON body");

    let ids = ids(&body);
    assert!(!ids.contains(&"general"));
    assert!(ids.contains(&"reviewer"));
    assert_eq!(ids.first(), Some(&"build"));
}
