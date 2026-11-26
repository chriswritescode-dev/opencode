use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use opencode_ai::route::compile::{StreamOptions, compile};
use opencode_ai::schema::{ContentPart, LlmRequest, Message, TextPart, UserMessage};
use opencode_ai::{ExecutionContext, MapEnv};
use opencode_catalog::builtin::Builtin;
use opencode_catalog::credential::Info;
use opencode_catalog::integration::{Attempt, AttemptStatus, Implementation, KeyInput, OAuthConnectInput};
use opencode_catalog::provider_plugin::poe;
use opencode_catalog::resolver::{self, Resolver};
use opencode_contract::generated::{
    ConnectionInfo, CredentialOAuth, CredentialOAuthType, CredentialValue, IntegrationEnvMethod,
    IntegrationEnvMethodType, IntegrationID, IntegrationKeyMethod, IntegrationKeyMethodType, IntegrationMethod,
    IntegrationMethodID, ModelRef, ProviderID,
};
use serde_json::json;
use url::Url;

use crate::builtin::{Harness, Input, start};
use crate::provider_plugin::support::{Reply, Routes, plugin_context_with, provider_settings};

const INTEGRATION_ID: &str = "poe";

const PROVIDER_ID: &str = "poe";

const METHOD_ID: &str = "browser";

const TOKEN: &str = "https://api.poe.com/token";

const OPENAI_COMPATIBLE: &str = "@opencode/ai/providers/openai-compatible";

const BASE_URL: &str = "https://api.poe.com/v1";

const MAX_EXPIRY: f64 = 8_640_000_000_000_000.0;

fn integration_id() -> IntegrationID {
    IntegrationID(INTEGRATION_ID.to_owned())
}

fn provider_id() -> ProviderID {
    ProviderID(PROVIDER_ID.to_owned())
}

fn method_id() -> IntegrationMethodID {
    IntegrationMethodID(METHOD_ID.to_owned())
}

fn client() -> reqwest::Client {
    opencode_config::http::client()
}

async fn seed_integration(harness: &mut Harness) {
    let id = integration_id();
    let registration = harness
        .services
        .integration
        .transform(move |editor| {
            editor.method_update(Implementation::Method {
                integration_id: id.clone(),
                method: IntegrationMethod::Key(IntegrationKeyMethod {
                    r#type: IntegrationKeyMethodType::Key,
                    label: None,
                    form: None,
                }),
            });
            editor.method_update(Implementation::Method {
                integration_id: id.clone(),
                method: IntegrationMethod::Env(IntegrationEnvMethod {
                    r#type: IntegrationEnvMethodType::Env,
                    names: vec!["POE_API_KEY".to_owned()],
                }),
            });
            Ok(())
        })
        .await;
    harness.registrations.registrations.push(registration);
}

async fn seed_provider(harness: &mut Harness) {
    let registration = harness
        .services
        .provider
        .transform(|editor| {
            let id = ProviderID(PROVIDER_ID.to_owned());
            let settings = provider_settings(BASE_URL);
            editor.update(&id, |provider| {
                provider.package = OPENAI_COMPATIBLE.to_owned();
                provider.settings = Some(settings.clone());
            });
            editor.models_update(&id, "test-model", |_| {});
            Ok(())
        })
        .await;
    harness.registrations.registrations.push(registration);
}

async fn add_plugin(harness: &mut Harness, routes: Arc<Routes>) {
    let context = plugin_context_with(harness, routes);
    let mut builtin = Builtin::default();
    poe::register(&harness.services, &context, &mut builtin).await;
    harness.record(builtin);
}

async fn harness(routes: Arc<Routes>) -> Harness {
    let mut harness = start(Input::new(Vec::new())).await;
    seed_integration(&mut harness).await;
    seed_provider(&mut harness).await;
    add_plugin(&mut harness, routes).await;
    harness
}

async fn connect(harness: &Harness) -> Attempt {
    harness
        .services
        .integration
        .oauth()
        .connect(OAuthConnectInput {
            integration_id: integration_id(),
            method_id: method_id(),
            answer: None,
            label: Some("Poe browser".to_owned()),
        })
        .await
        .expect("connect the Poe integration")
}

struct Login {
    attempt: Attempt,
    url: Url,
    callback: Url,
}

async fn login(harness: &Harness) -> Login {
    let attempt = connect(harness).await;
    let url = Url::parse(&attempt.url).expect("the Poe authorization url");
    let redirect = param(&url, "redirect_uri").expect("the Poe redirect");
    let mut callback = Url::parse(&redirect).expect("the Poe callback url");
    set_param(&mut callback, "state", &param(&url, "state").expect("the Poe state"));
    set_param(&mut callback, "code", "auth-code");
    Login { attempt, url, callback }
}

async fn key_credential(harness: &Harness, key: &str, label: &str) {
    harness
        .services
        .integration
        .connection()
        .key(KeyInput {
            integration_id: integration_id(),
            key: key.to_owned(),
            answer: None,
            label: Some(label.to_owned()),
        })
        .await
        .expect("create the Poe key credential");
}

async fn wait_for_status(harness: &Harness, attempt: &Attempt) -> AttemptStatus {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let status = harness
                .services
                .integration
                .oauth()
                .status(&integration_id(), &attempt.attempt_id)
                .await
                .expect("the Poe attempt status");
            if !matches!(status, AttemptStatus::Pending { .. }) {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the Poe attempt leaves pending")
}

fn credentials(harness: &Harness) -> Vec<Info> {
    harness.services.credentials.list(&integration_id()).expect("list the Poe credentials")
}

async fn active(harness: &Harness) -> Option<ConnectionInfo> {
    harness.services.integration.connection().active(&integration_id()).await.expect("the active Poe connection")
}

fn param(url: &Url, name: &str) -> Option<String> {
    url.query_pairs().find(|(key, _)| key == name).map(|(_, value)| value.into_owned())
}

fn set_param(url: &mut Url, name: &str, value: &str) {
    let mut pairs: Vec<(String, String)> =
        url.query_pairs().map(|(key, value)| (key.into_owned(), value.into_owned())).collect();
    match pairs.iter_mut().find(|(key, _)| key == name) {
        Some((_, existing)) => *existing = value.to_owned(),
        None => pairs.push((name.to_owned(), value.to_owned())),
    }
    url.set_query(None);
    url.query_pairs_mut().extend_pairs(pairs);
}

fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()).as_ref())
}

fn form(request: &opencode_ai::route::executor::PreparedHttp) -> Vec<(String, String)> {
    url::form_urlencoded::parse(&request.body).map(|(key, value)| (key.into_owned(), value.into_owned())).collect()
}

fn form_param(fields: &[(String, String)], name: &str) -> Option<String> {
    fields.iter().find(|(key, _)| key == name).map(|(_, value)| value.clone())
}

fn header(request: &opencode_ai::route::executor::PreparedHttp, name: &str) -> Option<String> {
    request.headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.clone())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registers_poe_browser_oauth_alongside_generic_key_and_environment_methods_without_fetching() {
    let routes = Routes::new();
    let harness = harness(routes.clone()).await;
    let integration = harness
        .services
        .integration
        .get(&integration_id())
        .await
        .expect("get the Poe integration")
        .expect("the Poe integration");
    assert_eq!(integration.methods.len(), 3);
    assert!(
        matches!(&integration.methods[0], IntegrationMethod::Key(key) if key.r#type == IntegrationKeyMethodType::Key)
    );
    assert!(matches!(&integration.methods[1], IntegrationMethod::Env(env) if env.names == ["POE_API_KEY"]));
    let IntegrationMethod::Oauth(oauth) = &integration.methods[2] else { panic!("the Poe browser method is last") };
    assert_eq!(oauth.id, method_id());
    assert_eq!(oauth.label, "Login with Poe (browser)");
    assert!(routes.requests().is_empty());
}

enum Expiry {
    Seconds(f64),
    Null,
    Missing,
}

impl Expiry {
    fn value(&self) -> Option<serde_json::Value> {
        match self {
            Self::Seconds(seconds) => Some(json!(seconds)),
            Self::Null => Some(serde_json::Value::Null),
            Self::Missing => None,
        }
    }
}

async fn exchange_login(expiry: Expiry) {
    let routes = Routes::new();
    let harness = harness(routes.clone()).await;
    key_credential(&harness, "previous-key", "Previous account").await;
    let previous = active(&harness).await.expect("the previous Poe connection");

    let login = login(&harness).await;
    assert_eq!(active(&harness).await, Some(previous.clone()));
    assert_eq!(login.url.origin().ascii_serialization(), "https://poe.com");
    assert_eq!(login.url.path(), "/oauth/authorize");
    assert_eq!(param(&login.url, "response_type").as_deref(), Some("code"));
    assert_eq!(param(&login.url, "client_id").as_deref(), Some("client_728290227fc048cc9262091a1ea197ea"));
    assert_eq!(param(&login.url, "scope").as_deref(), Some("apikey:create"));
    assert_eq!(param(&login.url, "code_challenge_method").as_deref(), Some("S256"));
    assert_eq!(login.callback.host_str(), Some("127.0.0.1"));
    assert_eq!(login.callback.path(), "/callback");
    assert!(param(&login.url, "state").is_some_and(|state| !state.is_empty()));

    let mut callback = login.callback.clone();
    let mut token = json!({ "api_key": " poe-key " });
    if let Some(value) = expiry.value() {
        set_param(&mut callback, "iss", "https://poe.com");
        token["api_key_expires_in"] = value;
    }
    routes.route_json("POST", TOKEN, token);

    let mut stray = login.callback.clone();
    set_param(&mut stray, "state", "another-login");
    let response = client().get(stray.as_str()).send().await.expect("fetch the Poe callback");
    assert_eq!(response.status(), 400);

    let before = opencode_util::time::now_ms();
    let response = client().get(callback.as_str()).send().await.expect("fetch the Poe callback");
    let after = opencode_util::time::now_ms();
    assert_eq!(response.status(), 200);
    let status = wait_for_status(&harness, &login.attempt).await;
    assert!(matches!(status, AttemptStatus::Complete { .. }), "got {status:?}");

    let exchange = routes.requests().first().expect("the Poe token exchange").clone();
    assert_eq!(exchange.url, TOKEN);
    assert!(header(&exchange, "content-type").is_some_and(|value| value.contains("application/x-www-form-urlencoded")));
    let fields = form(&exchange);
    assert_eq!(form_param(&fields, "grant_type").as_deref(), Some("authorization_code"));
    assert_eq!(form_param(&fields, "client_id").as_deref(), Some("client_728290227fc048cc9262091a1ea197ea"));
    assert_eq!(form_param(&fields, "code").as_deref(), Some("auth-code"));
    assert_eq!(form_param(&fields, "redirect_uri"), param(&login.url, "redirect_uri"));
    let verifier = form_param(&fields, "code_verifier").expect("the Poe code verifier");
    assert_eq!(param(&login.url, "code_challenge").as_deref(), Some(challenge(&verifier).as_str()));

    let saved = credentials(&harness);
    assert_eq!(saved.len(), 2);
    let active_credential = saved.iter().find(|credential| credential.label == "Poe browser").expect("the Poe login");
    let CredentialValue::Oauth(value) = &active_credential.value else { panic!("the Poe login is OAuth") };
    assert_eq!(value.method_id, method_id());
    assert_eq!(value.access, "poe-key");
    assert_eq!(value.refresh, "");
    match expiry {
        Expiry::Seconds(seconds) => {
            assert!(value.expires >= before as f64 + seconds * 1000.0);
            assert!(value.expires <= after as f64 + seconds * 1000.0);
        }
        Expiry::Null | Expiry::Missing => assert_eq!(value.expires, MAX_EXPIRY),
    }
    let connection = active(&harness).await.expect("the Poe connection");
    assert!(matches!(&connection, ConnectionInfo::Credential(connection) if connection.label == "Poe browser"));

    assert_inference_uses(&harness, "poe-key").await;

    harness
        .services
        .integration
        .oauth()
        .complete(&integration_id(), &login.attempt.attempt_id, None)
        .await
        .expect("complete the Poe attempt again");
    assert_eq!(credentials(&harness), saved);
}

async fn assert_inference_uses(harness: &Harness, token: &str) {
    let resolver = Resolver::new(
        harness.services.provider.clone(),
        harness.services.model.clone(),
        harness.services.integration.clone(),
        Arc::new(MapEnv::default()),
        Arc::new(resolver::NoBridge),
    );
    let reference = ModelRef { id: "test-model".to_owned(), provider_id: provider_id(), variant: None };
    let resolved = resolver.resolve(Some(&reference)).await.expect("resolve the Poe model").expect("the Poe model");
    assert_eq!(resolved.model.route.id, "openai-compatible-chat");
    let request = LlmRequest {
        messages: vec![Message::User(UserMessage {
            id: None,
            content: vec![ContentPart::Text(TextPart {
                text: "Hello".to_owned(),
                cache: None,
                provider_metadata: None,
            })],
            provider_metadata: None,
        })],
        ..LlmRequest::default()
    };
    let compiled = compile(&resolved.model, request, &StreamOptions::default(), &ExecutionContext::system())
        .expect("compile the Poe request");
    assert_eq!(compiled.prepared.request.url, "https://api.poe.com/v1/chat/completions");
    let authorization = header(&compiled.prepared.request, "authorization");
    let expected = format!("Bearer {token}");
    assert_eq!(authorization.as_deref(), Some(expected.as_str()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exchanges_a_pkce_code_for_a_native_poe_credential_with_expiry() {
    exchange_login(Expiry::Seconds(3600.0)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exchanges_a_pkce_code_for_a_native_poe_credential_with_null_expiry() {
    exchange_login(Expiry::Null).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exchanges_a_pkce_code_for_a_native_poe_credential_without_expiry() {
    exchange_login(Expiry::Missing).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completes_a_login_from_a_pasted_redirect_url_and_keeps_waiting_after_a_wrong_paste() {
    let routes = Routes::new();
    let harness = harness(routes.clone()).await;
    let login = login(&harness).await;

    let mut foreign = login.callback.clone();
    set_param(&mut foreign, "state", "another-login");
    let error = harness
        .services
        .integration
        .oauth()
        .complete(&integration_id(), &login.attempt.attempt_id, Some(foreign.as_str()))
        .await
        .expect_err("a foreign paste is rejected");
    assert_eq!(error.to_string(), "This URL belongs to a different sign-in");
    let status = harness
        .services
        .integration
        .oauth()
        .status(&integration_id(), &login.attempt.attempt_id)
        .await
        .expect("the Poe attempt status");
    assert!(matches!(status, AttemptStatus::Pending { .. }));

    routes.route_json("POST", TOKEN, json!({ "api_key": "poe-key", "api_key_expires_in": 3600 }));
    harness
        .services
        .integration
        .oauth()
        .complete(&integration_id(), &login.attempt.attempt_id, Some(login.callback.as_str()))
        .await
        .expect("complete the Poe attempt from a paste");
    let status = wait_for_status(&harness, &login.attempt).await;
    assert!(matches!(status, AttemptStatus::Complete { .. }), "got {status:?}");
    let exchange = routes.requests().first().expect("the Poe token exchange").clone();
    assert_eq!(form_param(&form(&exchange), "code").as_deref(), Some("auth-code"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn isolates_overlapping_login_attempts_and_closes_cancelled_listeners() {
    let routes = Routes::new();
    let harness = harness(routes.clone()).await;
    let first = login(&harness).await;
    let next = login(&harness).await;
    assert_ne!(first.callback.origin(), next.callback.origin());
    assert_ne!(param(&first.url, "code_challenge"), param(&next.url, "code_challenge"));
    assert_ne!(param(&first.url, "state"), param(&next.url, "state"));

    let mut crossed = first.callback.clone();
    set_param(&mut crossed, "state", &param(&next.url, "state").expect("the next state"));
    let response = client().get(crossed.as_str()).send().await.expect("fetch the first callback");
    assert_eq!(response.status(), 400);
    let status = harness
        .services
        .integration
        .oauth()
        .status(&integration_id(), &first.attempt.attempt_id)
        .await
        .expect("the first Poe attempt status");
    assert!(matches!(status, AttemptStatus::Pending { .. }));
    let status = harness
        .services
        .integration
        .oauth()
        .status(&integration_id(), &next.attempt.attempt_id)
        .await
        .expect("the next Poe attempt status");
    assert!(matches!(status, AttemptStatus::Pending { .. }));

    harness.services.integration.oauth().cancel(&integration_id(), &next.attempt.attempt_id).await;
    assert!(client().get(next.callback.as_str()).send().await.is_err());
    assert!(routes.requests().is_empty());
    assert_eq!(credentials(&harness), []);
}

async fn rejects_callback(params: &[(&str, &str)], message: &str) {
    let routes = Routes::new();
    let harness = harness(routes.clone()).await;
    let login = login(&harness).await;
    let mut callback = login.callback.clone();
    for (name, value) in params {
        set_param(&mut callback, name, value);
    }
    let response = client().get(callback.as_str()).send().await.expect("fetch the Poe callback");
    assert_eq!(response.status(), 400);
    assert!(response.text().await.expect("the Poe callback page").contains("Authorization failed"));
    let status = wait_for_status(&harness, &login.attempt).await;
    let AttemptStatus::Failed { message: failed, .. } = status else {
        panic!("the Poe attempt fails with {message}, got {status:?}")
    };
    assert_eq!(failed, *message);
    assert!(routes.requests().is_empty());
    assert_eq!(credentials(&harness), []);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_invalid_or_denied_callbacks() {
    rejects_callback(&[("iss", "https://other.example"), ("error", "access_denied")], "Invalid OAuth issuer").await;
    rejects_callback(&[("iss", "https://poe.com/")], "Invalid OAuth issuer").await;
    rejects_callback(&[("iss", "")], "Invalid OAuth issuer").await;
    rejects_callback(&[("code", "")], "Missing authorization code").await;
    rejects_callback(
        &[("error", "access_denied"), ("error_description", "User declined access")],
        "User declined access",
    )
    .await;
}

async fn preserves_connection(status: u16, body: &str, message: &str) {
    let routes = Routes::new();
    let harness = harness(routes.clone()).await;
    key_credential(&harness, "previous-key", "Previous account").await;
    let previous = active(&harness).await.expect("the previous Poe connection");
    let saved = credentials(&harness);
    let login = login(&harness).await;
    let body = body.to_owned();
    routes.route_with("POST", TOKEN, move |_| Reply { status, body: body.as_bytes().to_vec() });

    let response = client().get(login.callback.as_str()).send().await.expect("fetch the Poe callback");
    assert_eq!(response.status(), 400);
    assert!(response.text().await.expect("the Poe callback page").contains(message));
    let status = wait_for_status(&harness, &login.attempt).await;
    let AttemptStatus::Failed { message: failed, .. } = status else {
        panic!("the Poe attempt fails with {message}, got {status:?}")
    };
    assert_eq!(failed, message);
    assert_eq!(credentials(&harness), saved);
    assert_eq!(active(&harness).await, Some(previous));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preserves_the_active_connection_after_a_failed_token_exchange() {
    preserves_connection(
        400,
        r#"{"error":"invalid_grant","error_description":"Code expired"}"#,
        "Poe token exchange failed: Code expired",
    )
    .await;
    preserves_connection(400, r#"{"error":"invalid_grant"}"#, "Poe token exchange failed: invalid_grant").await;
    preserves_connection(502, "Bad gateway", "Poe token exchange failed (502)").await;
    preserves_connection(400, r#"{"error_description":"Rejected auth-code"}"#, "Poe token exchange failed (400)").await;
    for body in [
        "{}",
        "{",
        r#"{"api_key":"   "}"#,
        r#"{"api_key":"poe-key","api_key_expires_in":-1}"#,
        r#"{"api_key":"poe-key","api_key_expires_in":1.5}"#,
        r#"{"api_key":"poe-key","api_key_expires_in":1e309}"#,
    ] {
        preserves_connection(200, body, "Invalid Poe token response").await;
    }
    preserves_connection(
        200,
        r#"{"api_key":"poe-key","api_key_expires_in":8640000000000}"#,
        "Invalid Poe API key expiry",
    )
    .await;
}

async fn consumes_callback_once(cancel: bool) {
    let routes = Routes::new();
    let harness = harness(routes.clone()).await;
    let login = login(&harness).await;
    routes.route_json("POST", TOKEN, json!({ "api_key": "poe-key" }));
    routes.hold("POST", TOKEN);
    let page = tokio::spawn({
        let callback = login.callback.clone();
        async move { client().get(callback.as_str()).send().await }
    });
    routes.arrived().await;
    let status = harness
        .services
        .integration
        .oauth()
        .status(&integration_id(), &login.attempt.attempt_id)
        .await
        .expect("the Poe attempt status");
    assert!(matches!(status, AttemptStatus::Pending { .. }));
    assert_eq!(credentials(&harness), []);
    let second = client().get(login.callback.as_str()).send().await.expect("fetch the Poe callback again");
    assert_eq!(second.status(), 409);
    assert_eq!(routes.requests().len(), 1);

    if cancel {
        harness.services.integration.oauth().cancel(&integration_id(), &login.attempt.attempt_id).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        routes.set_json("POST", TOKEN, json!({ "api_key": "cancelled-key" }));
        routes.release();
        let page = tokio::time::timeout(Duration::from_secs(2), page)
            .await
            .expect("the cancelled callback page settles")
            .expect("the callback task joins");
        assert!(page.is_err());
        assert_eq!(credentials(&harness), []);
        return;
    }

    routes.release();
    let response = tokio::time::timeout(Duration::from_secs(2), page)
        .await
        .expect("the callback page settles")
        .expect("the callback task joins")
        .expect("the callback page responds");
    assert_eq!(response.status(), 200);
    assert!(response.text().await.expect("the Poe callback page").contains("Authorization successful"));
    let status = wait_for_status(&harness, &login.attempt).await;
    assert!(matches!(status, AttemptStatus::Complete { .. }), "got {status:?}");
    assert_eq!(credentials(&harness).len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn waits_for_the_token_exchange_and_consumes_the_callback_once() {
    consumes_callback_once(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn waits_for_the_token_exchange_and_closes_a_cancelled_callback() {
    consumes_callback_once(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keeps_near_expiry_keys_usable_and_requires_a_new_login_after_expiry() {
    let routes = Routes::new();
    let harness = harness(routes.clone()).await;
    let now = opencode_util::time::now_ms() as f64;
    harness
        .services
        .credentials
        .create(opencode_catalog::credential::CreateInput {
            id: None,
            integration_id: integration_id(),
            value: CredentialValue::Oauth(CredentialOAuth {
                r#type: CredentialOAuthType::Oauth,
                method_id: method_id(),
                access: "poe-key".to_owned(),
                refresh: String::new(),
                expires: now + 120_000.0,
                metadata: None,
            }),
            label: None,
            activate: None,
        })
        .expect("create the near-expiry Poe credential");
    let connection = harness
        .services
        .integration
        .connection()
        .active(&integration_id())
        .await
        .expect("the active Poe connection")
        .expect("the Poe connection");
    let resolved = harness
        .services
        .integration
        .connection()
        .resolve(&connection)
        .await
        .expect("resolve the near-expiry Poe credential")
        .expect("the Poe credential");
    let CredentialValue::Oauth(value) = resolved else { panic!("the Poe credential is OAuth") };
    assert_eq!(value.access, "poe-key");

    harness
        .services
        .credentials
        .create(opencode_catalog::credential::CreateInput {
            id: None,
            integration_id: integration_id(),
            value: CredentialValue::Oauth(CredentialOAuth {
                r#type: CredentialOAuthType::Oauth,
                method_id: method_id(),
                access: "expired-key".to_owned(),
                refresh: String::new(),
                expires: now - 120_000.0,
                metadata: None,
            }),
            label: None,
            activate: None,
        })
        .expect("create the expired Poe credential");
    let connection = harness
        .services
        .integration
        .connection()
        .active(&integration_id())
        .await
        .expect("the active Poe connection")
        .expect("the Poe connection");
    let error = harness
        .services
        .integration
        .connection()
        .resolve(&connection)
        .await
        .expect_err("the expired Poe credential requires a new login");
    assert_eq!(error.message(), "Poe API key expired. Log in with Poe again.");

    key_credential(&harness, "manual-key", "Manual").await;
    let connection = active(&harness).await.expect("the manual Poe connection");
    let resolved = harness
        .services
        .integration
        .connection()
        .resolve(&connection)
        .await
        .expect("resolve the manual Poe key")
        .expect("the manual Poe key");
    assert_eq!(
        resolved,
        CredentialValue::Key(opencode_contract::generated::CredentialKey {
            r#type: opencode_contract::generated::CredentialKeyType::Key,
            key: "manual-key".to_owned(),
            metadata: None,
        })
    );
    assert!(routes.requests().is_empty());
}
