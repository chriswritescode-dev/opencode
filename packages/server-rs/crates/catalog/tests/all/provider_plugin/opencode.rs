use std::sync::Arc;
use std::time::Duration;

use opencode_catalog::builtin::Builtin;
use opencode_catalog::credential::CreateInput;
use opencode_catalog::integration::{Attempt, AttemptError, AttemptStatus, OAuthConnectInput};
use opencode_catalog::provider_plugin::opencode;
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    CredentialOAuth, CredentialOAuthType, CredentialValue, FormAnswer, FormField1, FormStringField1Format,
    FormStringField1Type, FormValue, IntegrationID, IntegrationKeyMethodType, IntegrationMethod, IntegrationMethodID,
    IntegrationOAuthMethodType,
};
use serde_json::{Value, json};

use crate::builtin::{Harness, Input, start};
use crate::provider_plugin::support::{Reply, Routes, plugin_context_with};

const ORIGIN: &str = "https://console.example.com";

const USER_AGENT: &str = "opencode/test/test/opencode";

fn integration_id() -> IntegrationID {
    IntegrationID("opencode".to_owned())
}

fn method_id() -> IntegrationMethodID {
    IntegrationMethodID("device".to_owned())
}

async fn add_plugin(harness: &mut Harness, routes: Arc<Routes>) {
    let context = plugin_context_with(harness, routes);
    let mut builtin = Builtin::default();
    opencode::register(&harness.services, &context, &mut builtin).await;
    harness.record(builtin);
}

fn answer(server: FormValue) -> FormAnswer {
    JsonMap(vec![("server".to_owned(), server)])
}

async fn connect(harness: &Harness, answer: FormAnswer) -> Result<Attempt, AttemptError> {
    harness
        .services
        .integration
        .oauth()
        .connect(OAuthConnectInput {
            integration_id: integration_id(),
            method_id: method_id(),
            answer: Some(answer),
            label: None,
        })
        .await
}

async fn wait_for_status(harness: &Harness, attempt: &Attempt) -> AttemptStatus {
    for _ in 0..100 {
        let status = harness
            .services
            .integration
            .oauth()
            .status(&integration_id(), &attempt.attempt_id)
            .await
            .expect("the OpenCode attempt status");
        if !matches!(status, AttemptStatus::Pending { .. }) {
            return status;
        }
        tokio::time::advance(Duration::from_secs(10)).await;
        tokio::task::yield_now().await;
    }
    panic!("the OpenCode attempt leaves pending");
}

fn token(org_id: Option<Value>) -> Value {
    let mut token = json!({ "access_token": "access", "refresh_token": "refresh", "expires_in": 600 });
    if let Some(org_id) = org_id {
        token["org_id"] = org_id;
    }
    token
}

fn console_routes(server: &str, token: Value) -> Arc<Routes> {
    let routes = Routes::new();
    routes.route_json(
        "POST",
        &format!("{server}/auth/device/code"),
        json!({
            "device_code": "device",
            "user_code": "user",
            "verification_uri_complete": "/device?user_code=user",
            "expires_in": 60,
            "interval": 0,
        }),
    );
    routes.route_json("POST", &format!("{server}/auth/device/token"), token);
    routes.route_json("GET", &format!("{server}/api/user"), json!({ "id": "user", "email": "user@example.com" }));
    routes.route_json(
        "GET",
        &format!("{server}/api/orgs"),
        json!([{ "id": "org-z", "name": "Zebra" }, { "id": "org-a", "name": "Alpha" }]),
    );
    routes
}

fn request(routes: &Routes, method: &str, url: &str) -> opencode_ai::route::executor::PreparedHttp {
    routes
        .requests()
        .into_iter()
        .find(|request| request.method.eq_ignore_ascii_case(method) && request.url == url)
        .unwrap_or_else(|| panic!("the {method} {url} request"))
}

fn json_body(request: &opencode_ai::route::executor::PreparedHttp) -> Value {
    serde_json::from_slice(&request.body).expect("the request body is JSON")
}

fn header(request: &opencode_ai::route::executor::PreparedHttp, name: &str) -> Option<String> {
    request.headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.clone())
}

fn metadata<'a>(value: &'a CredentialOAuth, key: &str) -> Option<&'a Value> {
    value.metadata.as_ref()?.0.iter().find(|(name, _)| name == key).map(|(_, value)| value)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registers_account_and_service_account_methods() {
    let mut harness = start(Input::new(Vec::new())).await;
    add_plugin(&mut harness, Routes::new()).await;

    let integration = harness
        .services
        .integration
        .get(&integration_id())
        .await
        .expect("get the OpenCode integration")
        .expect("the OpenCode integration");
    assert_eq!(integration.name, "OpenCode");
    assert_eq!(integration.methods.len(), 2);

    let IntegrationMethod::Oauth(oauth) = &integration.methods[0] else { panic!("the device OAuth method is first") };
    assert_eq!(oauth.id, method_id());
    assert_eq!(oauth.r#type, IntegrationOAuthMethodType::Oauth);
    assert_eq!(oauth.label, "OpenCode account");
    let form = oauth.form.as_ref().expect("the device method has a form");
    assert_eq!(form.0.len(), 1);
    let FormField1::String(field) = &form.0[0] else { panic!("the server field is a string field") };
    assert_eq!(field.key, "server");
    assert_eq!(field.r#type, FormStringField1Type::String);
    assert_eq!(field.format, Some(FormStringField1Format::Uri));
    assert_eq!(field.hidden, Some(true));
    assert_eq!(field.default.as_deref(), Some("https://opencode.ai/console"));

    let IntegrationMethod::Key(key) = &integration.methods[1] else { panic!("the API key method is second") };
    assert_eq!(key.r#type, IntegrationKeyMethodType::Key);
    assert_eq!(key.label.as_deref(), Some("API key (service account)"));
}

#[tokio::test(start_paused = true)]
async fn uses_a_canonical_custom_server_throughout_device_authorization() {
    let server = format!("{ORIGIN}/console");
    let routes = Routes::new();
    routes.route_json(
        "POST",
        &format!("{server}/auth/device/code"),
        json!({
            "device_code": "device",
            "user_code": "user",
            "verification_uri_complete": "/console/device?user_code=user&client_id=opencode-cli",
            "expires_in": 60,
            "interval": 0,
        }),
    );
    routes.route_json(
        "POST",
        &format!("{server}/auth/device/token"),
        json!({ "access_token": "access", "refresh_token": "refresh", "expires_in": 600 }),
    );
    routes.route_json("GET", &format!("{server}/api/user"), json!({ "id": "user", "email": "user@example.com" }));
    routes.route_json("GET", &format!("{server}/api/orgs"), json!([{ "id": "org", "name": "Org" }]));

    let mut harness = start(Input::new(Vec::new())).await;
    add_plugin(&mut harness, routes.clone()).await;

    let attempt = connect(&harness, answer(FormValue::String(format!("{ORIGIN}/console///?ignored=true#ignored"))))
        .await
        .expect("start the device authorization");
    assert_eq!(attempt.url, format!("{server}/device?user_code=user&client_id=opencode-cli"));
    assert_eq!(attempt.instructions, "Enter code: user");
    assert!(!attempt.paste);

    let status = wait_for_status(&harness, &attempt).await;
    assert!(matches!(status, AttemptStatus::Complete { .. }), "got {status:?}");

    let device = request(&routes, "POST", &format!("{server}/auth/device/code"));
    assert_eq!(json_body(&device), json!({ "client_id": "opencode-cli", "supports_org_scope": true }));
    let poll = request(&routes, "POST", &format!("{server}/auth/device/token"));
    assert_eq!(
        json_body(&poll),
        json!({
            "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
            "device_code": "device",
            "client_id": "opencode-cli",
        })
    );
    let user = request(&routes, "GET", &format!("{server}/api/user"));
    assert_eq!(header(&user, "authorization").as_deref(), Some("Bearer access"));
    assert_eq!(header(&user, "accept").as_deref(), Some("application/json"));
    for request in routes.requests() {
        assert_eq!(header(&request, "user-agent").as_deref(), Some(USER_AGENT), "{}", request.url);
    }

    let credentials = harness.services.credentials.list(&integration_id()).expect("list the OpenCode credentials");
    assert_eq!(credentials.len(), 1);
    let CredentialValue::Oauth(value) = &credentials[0].value else { panic!("the OpenCode credential is OAuth") };
    assert_eq!(value.access, "access");
    assert_eq!(value.refresh, "refresh");
    assert_eq!(metadata(value, "server"), Some(&json!(server)));
    assert_eq!(metadata(value, "accountID"), Some(&json!("user")));
    assert_eq!(metadata(value, "email"), Some(&json!("user@example.com")));
    assert_eq!(metadata(value, "orgID"), Some(&json!("org")));
    assert_eq!(metadata(value, "orgName"), Some(&json!("Org")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_malformed_device_verification_urls() {
    let routes = Routes::new();
    routes.route_json(
        "POST",
        &format!("{ORIGIN}/auth/device/code"),
        json!({
            "device_code": "device",
            "user_code": "user",
            "verification_uri_complete": "http://[::1",
            "expires_in": 60,
            "interval": 0,
        }),
    );
    let mut harness = start(Input::new(Vec::new())).await;
    add_plugin(&mut harness, routes).await;

    let error = connect(&harness, answer(FormValue::String(ORIGIN.to_owned())))
        .await
        .expect_err("the malformed verification URL is rejected");
    assert!(error.to_string().contains("Invalid device verification URL"), "got {error}");
}

async fn authorizes_with_organization(org_id: Option<Value>, expected_org: &str, expected_name: &str) {
    let routes = console_routes(ORIGIN, token(org_id));
    let mut harness = start(Input::new(Vec::new())).await;
    add_plugin(&mut harness, routes).await;

    let attempt =
        connect(&harness, answer(FormValue::String(ORIGIN.to_owned()))).await.expect("start the device authorization");
    assert_eq!(attempt.url, format!("{ORIGIN}/device?user_code=user"));
    let status = wait_for_status(&harness, &attempt).await;
    assert!(matches!(status, AttemptStatus::Complete { .. }), "got {status:?}");

    let credentials = harness.services.credentials.list(&integration_id()).expect("list the OpenCode credentials");
    assert_eq!(credentials.len(), 1);
    assert_eq!(credentials[0].label, expected_name);
    let CredentialValue::Oauth(value) = &credentials[0].value else { panic!("the OpenCode credential is OAuth") };
    assert_eq!(metadata(value, "orgID"), Some(&json!(expected_org)));
    assert_eq!(metadata(value, "orgName"), Some(&json!(expected_name)));
}

#[tokio::test(start_paused = true)]
async fn uses_the_device_token_organization_during_authorization() {
    authorizes_with_organization(Some(json!("org-z")), "org-z", "Zebra").await;
}

#[tokio::test(start_paused = true)]
async fn uses_the_first_organization_by_name_without_a_device_token_organization() {
    authorizes_with_organization(None, "org-a", "Alpha").await;
    authorizes_with_organization(Some(json!(null)), "org-a", "Alpha").await;
}

#[tokio::test(start_paused = true)]
async fn reports_a_missing_device_token_organization() {
    let routes = console_routes(ORIGIN, token(Some(json!("org-missing"))));
    let mut harness = start(Input::new(Vec::new())).await;
    add_plugin(&mut harness, routes).await;

    let attempt =
        connect(&harness, answer(FormValue::String(ORIGIN.to_owned()))).await.expect("start the device authorization");
    let status = wait_for_status(&harness, &attempt).await;
    let AttemptStatus::Failed { message, .. } = status else { panic!("the attempt fails, got {status:?}") };
    assert_eq!(message, "OpenCode organization not found: org-missing");
    assert!(harness.services.credentials.list(&integration_id()).expect("list the OpenCode credentials").is_empty());
}

fn oauth(server: &str) -> CredentialValue {
    CredentialValue::Oauth(CredentialOAuth {
        r#type: CredentialOAuthType::Oauth,
        method_id: method_id(),
        access: "expired-access".to_owned(),
        refresh: "old-refresh".to_owned(),
        expires: 1.0,
        metadata: Some(JsonMap(vec![
            ("server".to_owned(), json!(server)),
            ("orgID".to_owned(), json!("org-a")),
            ("orgName".to_owned(), json!("Alpha")),
            ("custom".to_owned(), json!("preserved")),
        ])),
    })
}

async fn rotates_a_stored_credential(org_id: Option<Value>, expected: (&str, &str)) {
    let routes = Routes::new();
    routes.route_json("POST", &format!("{ORIGIN}/auth/device/token"), token(org_id));
    let mut harness = start(Input::new(Vec::new())).await;
    add_plugin(&mut harness, routes.clone()).await;
    let stored = harness
        .services
        .credentials
        .create(CreateInput {
            id: None,
            integration_id: integration_id(),
            value: oauth(ORIGIN),
            label: Some("Custom label".to_owned()),
            activate: None,
        })
        .expect("create the OpenCode credential");
    let connection = harness
        .services
        .integration
        .connection()
        .active(&integration_id())
        .await
        .expect("the active OpenCode connection")
        .expect("the OpenCode connection");

    let resolved = harness
        .services
        .integration
        .connection()
        .resolve(&connection)
        .await
        .expect("refresh the OpenCode credential")
        .expect("the OpenCode credential");
    let CredentialValue::Oauth(value) = &resolved else { panic!("the OpenCode credential is OAuth") };
    assert_eq!(value.access, "access");
    assert_eq!(value.refresh, "refresh");
    assert_eq!(metadata(value, "server"), Some(&json!(ORIGIN)));
    assert_eq!(metadata(value, "custom"), Some(&json!("preserved")));
    assert_eq!(metadata(value, "orgID"), Some(&json!(expected.0)));
    assert_eq!(metadata(value, "orgName"), Some(&json!(expected.1)));
    assert!(value.expires > opencode_util::time::now_ms() as f64);

    let refresh = request(&routes, "POST", &format!("{ORIGIN}/auth/device/token"));
    assert_eq!(
        json_body(&refresh),
        json!({ "grant_type": "refresh_token", "refresh_token": "old-refresh", "client_id": "opencode-cli" })
    );
    assert_eq!(header(&refresh, "user-agent").as_deref(), Some(USER_AGENT));

    let updated =
        harness.services.credentials.get(&stored.id).expect("get the OpenCode credential").expect("the credential");
    assert_eq!(updated.label, "Custom label");
    assert_eq!(updated.value, resolved);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn persists_rotated_credentials() {
    for (org_id, expected) in [
        (Some(json!("org-z")), ("org-z", "org-z")),
        (None, ("org-a", "Alpha")),
        (Some(json!(null)), ("org-a", "Alpha")),
        (Some(json!("org-missing")), ("org-missing", "org-missing")),
        (Some(json!("org-a")), ("org-a", "Alpha")),
    ] {
        rotates_a_stored_credential(org_id, expected).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_a_rejected_refresh_token_as_signed_out() {
    let routes = Routes::new();
    routes.route_with("POST", &format!("{ORIGIN}/auth/device/token"), |_| {
        Reply::json_status(json!({ "error": "invalid_grant", "error_description": "refresh token revoked" }), 400)
    });
    let mut harness = start(Input::new(Vec::new())).await;
    add_plugin(&mut harness, routes).await;
    let stored = harness
        .services
        .credentials
        .create(CreateInput {
            id: None,
            integration_id: integration_id(),
            value: oauth(ORIGIN),
            label: None,
            activate: None,
        })
        .expect("create the OpenCode credential");
    let connection = harness
        .services
        .integration
        .connection()
        .active(&integration_id())
        .await
        .expect("the active OpenCode connection")
        .expect("the OpenCode connection");

    let error = harness
        .services
        .integration
        .connection()
        .resolve(&connection)
        .await
        .expect_err("the rejected refresh token is signed out");
    assert_eq!(error.message(), "Reconnect OpenCode Console to continue.");
    let stored = harness.services.credentials.get(&stored.id).expect("get the credential").expect("the credential");
    assert_eq!(stored.value, oauth(ORIGIN));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_non_http_opencode_servers() {
    let mut harness = start(Input::new(Vec::new())).await;
    add_plugin(&mut harness, Routes::new()).await;

    let error = connect(&harness, answer(FormValue::String("ftp://console.example.com".to_owned())))
        .await
        .expect_err("the non-HTTP server is rejected");
    assert!(error.to_string().contains("Invalid OpenCode server URL: expected HTTP(S)"), "got {error}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_non_string_opencode_servers() {
    let mut harness = start(Input::new(Vec::new())).await;
    add_plugin(&mut harness, Routes::new()).await;

    let error = connect(&harness, answer(FormValue::Bool(true))).await.expect_err("the non-string server is rejected");
    assert!(error.to_string().contains("Expected string for form field: server"), "got {error}");
}
