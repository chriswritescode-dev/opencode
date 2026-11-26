use http::StatusCode;
use opencode_catalog::credential::CreateInput;
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    CredentialOAuth, CredentialOAuthType, CredentialValue, IntegrationID, IntegrationMethodID,
};
use serde_json::{Value, json};

use crate::{authorized, ready_app_with_services, services_at, temp_directory, text};

async fn body_json(response: opencode_http::response::Response) -> Value {
    serde_json::from_str(&text(response).await).expect("a JSON body")
}

fn signed_in(server: &str) -> CredentialValue {
    CredentialValue::Oauth(CredentialOAuth {
        r#type: CredentialOAuthType::Oauth,
        method_id: IntegrationMethodID("device".to_owned()),
        refresh: "console-refresh".to_owned(),
        access: "console-access".to_owned(),
        expires: 4_102_444_800_000.0,
        metadata: Some(JsonMap(vec![
            ("server".to_owned(), json!(server)),
            ("orgID".to_owned(), json!("wrk_anomaly")),
            ("orgName".to_owned(), json!("Anomaly")),
        ])),
    })
}

#[tokio::test]
async fn unsigned_location_has_no_usage_limits() {
    let project = temp_directory("usage-unsigned");
    let app = ready_app_with_services(services_at(&project));

    let response = app.handle(authorized("GET", "/api/experimental/usage/limits")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["location"]["directory"], project.to_string_lossy().as_ref());
    assert_eq!(body["data"], Value::Null);
}

#[tokio::test]
async fn console_failure_is_a_503() {
    let project = temp_directory("usage-failure");
    let services = services_at(&project);
    services
        .credentials
        .create(CreateInput {
            id: None,
            integration_id: IntegrationID("opencode".to_owned()),
            value: signed_in("not a url"),
            label: None,
            activate: None,
        })
        .expect("seed the OpenCode Console credential");
    let app = ready_app_with_services(services);

    let response = app.handle(authorized("GET", "/api/experimental/usage/limits")).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = body_json(response).await;
    assert_eq!(body["_tag"], "ServiceUnavailableError");
    assert_eq!(body["service"], "opencode");
    assert!(body["message"].as_str().expect("a message").contains("OpenCode Console"), "{body}");
}

#[tokio::test]
async fn invalid_query_is_a_400() {
    let project = temp_directory("usage-invalid");
    let app = ready_app_with_services(services_at(&project));

    let response = app.handle(authorized("GET", "/api/experimental/usage/limits?location=1")).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(body["_tag"], "InvalidRequestError");
    assert_eq!(body["kind"], "Query");
}
