use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bytes::Bytes;
use http::{Request, StatusCode};
use opencode_catalog::credential::CreateInput;
use opencode_catalog::integration::{
    AuthorizationError, Implementation, OAuthAuthorization, OAuthFuture, OAuthImplementation, RevokeCallback,
};
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    CredentialOAuth, CredentialOAuthType, CredentialValue, FormAnswer, IntegrationID, IntegrationMethodID,
    IntegrationOAuthMethod, IntegrationOAuthMethodType,
};
use opencode_http::app::App;
use opencode_location::location::LocationRef;
use serde_json::Value;

use crate::{authorized, basic, ready_app_with_services, services_at, temp_directory, text};

const DENY_ALL: &str = r#"{"policies":[{"action":"integration.connect","resource":"*","effect":"deny"}]}"#;

struct RevokeProbe {
    integration_id: IntegrationID,
    method: IntegrationOAuthMethod,
    revoked: Arc<AtomicBool>,
}

impl OAuthImplementation for RevokeProbe {
    fn integration_id(&self) -> &IntegrationID {
        &self.integration_id
    }

    fn method(&self) -> &IntegrationOAuthMethod {
        &self.method
    }

    fn authorize(&self, _answer: FormAnswer) -> OAuthFuture<OAuthAuthorization> {
        Box::pin(async { Err(AuthorizationError::new(std::io::Error::other("unused"))) })
    }

    fn revoke(&self) -> Option<RevokeCallback> {
        let revoked = self.revoked.clone();
        Some(Box::new(move |_| {
            let revoked = revoked.clone();
            Box::pin(async move {
                revoked.store(true, Ordering::SeqCst);
                Ok(())
            })
        }))
    }
}

fn json_request(method: &str, uri: &str, body: &str) -> Request<Bytes> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", basic("opencode", "pw"))
        .header("content-type", "application/json")
        .body(Bytes::from(body.to_owned()))
        .expect("a request")
}

async fn body_json(response: opencode_http::response::Response) -> Value {
    serde_json::from_str(&text(response).await).expect("a JSON body")
}

async fn create(app: &App, payload: &str) -> (StatusCode, Value) {
    let response = app.handle(json_request("POST", "/api/credential", payload)).await;
    let status = response.status();
    (status, body_json(response).await)
}

async fn list(app: &App) -> (StatusCode, Value) {
    let response = app.handle(authorized("GET", "/api/credential")).await;
    let status = response.status();
    (status, body_json(response).await)
}

fn entry<'a>(data: &'a [Value], id: &str) -> &'a Value {
    data.iter().find(|entry| entry["id"] == id).unwrap_or_else(|| panic!("credential {id}"))
}

fn write_deny_all(services: &opencode_http::services::GlobalServices) {
    std::fs::create_dir_all(&services.paths.config).expect("create the global config directory");
    std::fs::write(services.paths.config.join("opencode.json"), DENY_ALL).expect("write the global config");
}

#[tokio::test]
async fn create_list_update_activate_and_remove_credentials() {
    let services = services_at(&temp_directory("credential-lifecycle"));
    std::fs::create_dir_all(&services.paths.config).expect("create the global config directory");
    let app = ready_app_with_services(services);

    let (status, created) = create(&app, r#"{"integrationID":"custom","value":{"type":"key","key":"sk-one"}}"#).await;
    assert_eq!(status, StatusCode::OK);
    let first = created["data"]["id"].as_str().expect("an id").to_owned();
    assert!(first.starts_with("cred_"));
    assert_eq!(created["data"]["integrationID"], "custom");
    assert_eq!(created["data"]["label"], "default");
    assert_eq!(created["data"]["active"], true);

    let (status, created) =
        create(&app, r#"{"integrationID":"custom","label":"second","value":{"type":"key","key":"sk-two"}}"#).await;
    assert_eq!(status, StatusCode::OK);
    let second = created["data"]["id"].as_str().expect("an id").to_owned();
    assert_eq!(created["data"]["label"], "second");
    assert_eq!(created["data"]["active"], true);

    let (status, listed) = list(&app).await;
    assert_eq!(status, StatusCode::OK);
    let data = listed["data"].as_array().expect("a credential list");
    assert_eq!(data.len(), 2);
    assert_eq!(entry(data, &second)["active"], true);
    assert_eq!(entry(data, &first)["active"], false);

    let response =
        app.handle(json_request("PATCH", &format!("/api/credential/{first}"), r#"{"label":"renamed"}"#)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(text(response).await, "");

    let (status, listed) = list(&app).await;
    assert_eq!(status, StatusCode::OK);
    let data = listed["data"].as_array().expect("a credential list");
    assert_eq!(entry(data, &first)["label"], "renamed");

    let response = app.handle(authorized("POST", &format!("/api/credential/{first}/activate"))).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(text(response).await, "");

    let (status, listed) = list(&app).await;
    assert_eq!(status, StatusCode::OK);
    let data = listed["data"].as_array().expect("a credential list");
    assert_eq!(entry(data, &first)["active"], true);
    assert_eq!(entry(data, &second)["active"], false);

    let response = app.handle(authorized("DELETE", &format!("/api/credential/{second}"))).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(text(response).await, "");

    let (status, listed) = list(&app).await;
    assert_eq!(status, StatusCode::OK);
    let data = listed["data"].as_array().expect("a credential list");
    assert_eq!(data.len(), 1);
    assert_eq!(entry(data, &first)["id"], first.as_str());

    let response = app.handle(authorized("DELETE", &format!("/api/credential/{second}"))).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn create_rejects_an_existing_id() {
    let services = services_at(&temp_directory("credential-conflict"));
    std::fs::create_dir_all(&services.paths.config).expect("create the global config directory");
    let app = ready_app_with_services(services);

    let payload = r#"{"id":"cred_fixed","integrationID":"custom","value":{"type":"key","key":"sk-fixed"}}"#;
    let (status, _) = create(&app, payload).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = create(&app, payload).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["_tag"], "ConflictError");
    assert_eq!(body["resource"], "cred_fixed");
    assert_eq!(body["message"], "Credential already exists: cred_fixed");
}

#[tokio::test]
async fn create_is_denied_by_integration_policy() {
    let services = services_at(&temp_directory("credential-policy"));
    write_deny_all(&services);
    let app = ready_app_with_services(services);

    let (status, body) =
        create(&app, r#"{"integrationID":"anthropic","value":{"type":"key","key":"sk-denied"}}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["_tag"], "InvalidRequestError");
    assert_eq!(body["kind"], "integration_policy");

    let (status, listed) = list(&app).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed, serde_json::json!({ "data": [] }));
}

#[tokio::test]
async fn mcp_integrations_skip_integration_policy() {
    let services = services_at(&temp_directory("credential-mcp"));
    write_deny_all(&services);
    let app = ready_app_with_services(services.clone());
    let reference = LocationRef::new(services.paths.config.to_string_lossy().into_owned(), None);
    let lease = services.locations.get(&reference).await.expect("boot the global config location");
    let _registration = lease
        .services()
        .integrations
        .transform(|editor| {
            editor.update(&IntegrationID("mcp_test".to_owned()), |info| {
                info.metadata = Some(JsonMap(vec![("source".to_owned(), serde_json::json!("mcp"))]));
            });
            Ok(())
        })
        .await;

    let (status, created) = create(&app, r#"{"integrationID":"mcp_test","value":{"type":"key","key":"sk-mcp"}}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(created["data"]["integrationID"], "mcp_test");

    let (status, body) =
        create(&app, r#"{"integrationID":"anthropic","value":{"type":"key","key":"sk-denied"}}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["kind"], "integration_policy");
}

#[tokio::test]
async fn remove_revokes_through_the_integration_service() {
    let services = services_at(&temp_directory("credential-revoke"));
    std::fs::create_dir_all(&services.paths.config).expect("create the global config directory");
    let app = ready_app_with_services(services.clone());
    let reference = LocationRef::new(services.paths.config.to_string_lossy().into_owned(), None);
    let lease = services.locations.get(&reference).await.expect("boot the global config location");

    let integration_id = IntegrationID("revoke_test".to_owned());
    let method = IntegrationOAuthMethod {
        id: IntegrationMethodID("revoke_test".to_owned()),
        r#type: IntegrationOAuthMethodType::Oauth,
        label: "Revoke Test".to_owned(),
        form: None,
    };
    let revoked = Arc::new(AtomicBool::new(false));
    let probe = Arc::new(RevokeProbe {
        integration_id: integration_id.clone(),
        method: method.clone(),
        revoked: revoked.clone(),
    });
    let _registration = lease
        .services()
        .integrations
        .transform(move |editor| {
            editor.method_update(Implementation::OAuth(probe.clone()));
            Ok(())
        })
        .await;

    let created = services
        .credentials
        .create(CreateInput {
            id: None,
            integration_id,
            value: CredentialValue::Oauth(CredentialOAuth {
                r#type: CredentialOAuthType::Oauth,
                method_id: IntegrationMethodID("revoke_test".to_owned()),
                refresh: "refresh".to_owned(),
                access: "access".to_owned(),
                expires: 0.0,
                metadata: None,
            }),
            label: None,
            activate: None,
        })
        .expect("create the oauth credential");

    let response = app.handle(authorized("DELETE", &format!("/api/credential/{}", created.id.0))).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(revoked.load(Ordering::SeqCst));
    assert!(services.credentials.get(&created.id).expect("read the credential").is_none());
}

#[tokio::test]
async fn invalid_bodies_and_params_are_400() {
    let services = services_at(&temp_directory("credential-invalid"));
    std::fs::create_dir_all(&services.paths.config).expect("create the global config directory");
    let app = ready_app_with_services(services);

    let response = app.handle(json_request("POST", "/api/credential", "{")).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(body["_tag"], "InvalidRequestError");
    assert_eq!(body["kind"], "Payload");

    let response = app.handle(json_request("PATCH", "/api/credential/x", "{")).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(body["_tag"], "InvalidRequestError");
    assert_eq!(body["kind"], "Payload");
}
