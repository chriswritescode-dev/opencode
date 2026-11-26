use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use opencode_catalog::builtin::Builtin;
use opencode_catalog::credential::{CreateInput, Info};
use opencode_catalog::integration::{Attempt, AttemptStatus, OAuthConnectInput};
use opencode_catalog::provider_plugin::xai;
use opencode_contract::generated::{
    CredentialOAuth, CredentialOAuthType, CredentialValue, IntegrationID, IntegrationKeyMethodType, IntegrationMethod,
    IntegrationMethodID, ProviderID, ProviderInfoActivation,
};
use serde_json::{Value, json};

use crate::builtin::{Harness, Input, start};
use crate::provider_plugin::support::{Reply, Routes, plugin_context_with, provider_settings_from, settings_json};

const INTEGRATION_ID: &str = "xai";

const PROVIDER_ID: &str = "xai";

const DEVICE: &str = "device";

const BROWSER: &str = "browser";

const DEVICE_CODE_URL: &str = "https://auth.x.ai/oauth2/device/code";

const TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";

const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";

fn integration_id() -> IntegrationID {
    IntegrationID(INTEGRATION_ID.to_owned())
}

fn provider_id() -> ProviderID {
    ProviderID(PROVIDER_ID.to_owned())
}

fn method_id() -> IntegrationMethodID {
    IntegrationMethodID(DEVICE.to_owned())
}

fn oauth(method: &str, access: &str, refresh: &str, expires: f64) -> CredentialValue {
    CredentialValue::Oauth(CredentialOAuth {
        r#type: CredentialOAuthType::Oauth,
        method_id: IntegrationMethodID(method.to_owned()),
        refresh: refresh.to_owned(),
        access: access.to_owned(),
        expires,
        metadata: None,
    })
}

async fn add_plugin(harness: &mut Harness, routes: Arc<Routes>) {
    let context = plugin_context_with(harness, routes);
    let mut builtin = Builtin::default();
    xai::register(&harness.services, &context, &mut builtin).await;
    harness.record(builtin);
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
            label: None,
        })
        .await
        .expect("connect the xAI integration")
}

async fn wait_for_status(harness: &Harness, attempt: &Attempt) -> AttemptStatus {
    for _ in 0..100 {
        let status = harness
            .services
            .integration
            .oauth()
            .status(&integration_id(), &attempt.attempt_id)
            .await
            .expect("the xAI attempt status");
        if !matches!(status, AttemptStatus::Pending { .. }) {
            return status;
        }
        tokio::time::advance(Duration::from_secs(10)).await;
        tokio::task::yield_now().await;
    }
    panic!("the xAI attempt leaves pending")
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

fn model_settings(model: &opencode_contract::generated::ModelInfo) -> Value {
    model
        .settings
        .as_ref()
        .map(|settings| serde_json::from_str(&opencode_contract::codec::encode_json(settings)).expect("json"))
        .unwrap_or(Value::Null)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registers_device_oauth_and_api_key_methods() {
    let mut harness = start(Input::new(Vec::new())).await;
    add_plugin(&mut harness, Routes::new()).await;
    let integration = harness
        .services
        .integration
        .get(&integration_id())
        .await
        .expect("get the xAI integration")
        .expect("the xAI integration");
    assert_eq!(integration.name, "xAI");
    assert_eq!(integration.methods.len(), 2);
    let IntegrationMethod::Oauth(oauth) = &integration.methods[0] else { panic!("the device OAuth method is first") };
    assert_eq!(oauth.id, method_id());
    assert_eq!(oauth.label, "SuperGrok Subscription");
    let IntegrationMethod::Key(key) = &integration.methods[1] else { panic!("the API key method is second") };
    assert_eq!(key.r#type, IntegrationKeyMethodType::Key);
    assert_eq!(key.label.as_deref(), Some("Manually enter API Key"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn migrates_browser_oauth_credentials_to_the_device_method() {
    let mut harness = start(Input::new(Vec::new())).await;
    let original = harness
        .services
        .credentials
        .create(CreateInput {
            id: None,
            integration_id: integration_id(),
            value: CredentialValue::Oauth(CredentialOAuth {
                r#type: CredentialOAuthType::Oauth,
                method_id: IntegrationMethodID(BROWSER.to_owned()),
                access: "access".to_owned(),
                refresh: "refresh".to_owned(),
                expires: 123.0,
                metadata: Some(opencode_contract::codec::JsonMap(vec![("account".to_owned(), json!("account"))])),
            }),
            label: None,
            activate: None,
        })
        .expect("create the browser credential");
    add_plugin(&mut harness, Routes::new()).await;
    let migrated = harness
        .services
        .credentials
        .get(&original.id)
        .expect("get the migrated xAI credential")
        .expect("the migrated xAI credential");
    assert_eq!(
        migrated,
        Info {
            value: CredentialValue::Oauth(CredentialOAuth {
                r#type: CredentialOAuthType::Oauth,
                method_id: method_id(),
                access: "access".to_owned(),
                refresh: "refresh".to_owned(),
                expires: 123.0,
                metadata: Some(opencode_contract::codec::JsonMap(vec![("account".to_owned(), json!("account"))])),
            }),
            ..original
        }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enables_xai_responses_websockets() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = harness
        .services
        .provider
        .transform(|editor| {
            let id = ProviderID(PROVIDER_ID.to_owned());
            editor.update(&id, |provider| {
                provider.package = "@opencode/ai/providers/xai".to_owned();
                provider.activation = ProviderInfoActivation::Enabled;
            });
            editor.models_update(&id, "grok-4.6", |_| {});
            Ok(())
        })
        .await;
    add_plugin(&mut harness, Routes::new()).await;
    let provider = harness.services.provider.get(&provider_id()).await.unwrap().expect("the xAI provider");
    assert_eq!(
        settings_json(&provider).and_then(|settings| settings.get("transport").cloned()),
        Some(json!("websocket"))
    );
    let model = harness.services.model.get(&provider_id(), "grok-4.6").await.unwrap().expect("the xAI model");
    assert_eq!(model_settings(&model)["transport"], Value::Null);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keeps_a_configured_transport() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = harness
        .services
        .provider
        .transform(|editor| {
            let id = ProviderID(PROVIDER_ID.to_owned());
            let settings = provider_settings_from(json!({ "transport": "http" }));
            editor.update(&id, |provider| provider.settings = Some(settings.clone()));
            Ok(())
        })
        .await;
    add_plugin(&mut harness, Routes::new()).await;
    let provider = harness.services.provider.get(&provider_id()).await.unwrap().expect("the xAI provider");
    assert_eq!(settings_json(&provider).and_then(|settings| settings.get("transport").cloned()), Some(json!("http")));
}

#[tokio::test(start_paused = true)]
async fn completes_a_device_authorization() {
    let routes = Routes::new();
    routes.route_json(
        "POST",
        DEVICE_CODE_URL,
        json!({
            "device_code": "device-code",
            "user_code": "ABCD-EFGH",
            "verification_uri": "https://accounts.x.ai/device",
            "verification_uri_complete": "https://accounts.x.ai/device?user_code=ABCD-EFGH",
            "expires_in": 600,
            "interval": 0.001,
        }),
    );
    let polls = Arc::new(AtomicUsize::new(0));
    routes.route_with("POST", TOKEN_URL, {
        let polls = polls.clone();
        move |_| {
            if polls.fetch_add(1, Ordering::SeqCst) == 0 {
                return Reply::json_status(json!({ "error": "authorization_pending" }), 400);
            }
            Reply::json(json!({ "access_token": "access", "refresh_token": "refresh", "expires_in": 3600 }))
        }
    });
    let mut harness = start(Input::new(Vec::new())).await;
    add_plugin(&mut harness, routes.clone()).await;

    let before = opencode_util::time::now_ms();
    let attempt = connect(&harness).await;
    assert_eq!(attempt.url, "https://accounts.x.ai/device?user_code=ABCD-EFGH");
    assert_eq!(attempt.instructions, "Open https://accounts.x.ai/device on any device and enter code: ABCD-EFGH");
    assert!(attempt.time.expires >= before + 600_000);
    assert!(attempt.time.expires <= opencode_util::time::now_ms() + 600_000);
    assert!(!attempt.paste);

    let device = routes.requests().first().expect("the xAI device request").clone();
    assert_eq!(device.url, DEVICE_CODE_URL);
    assert!(header(&device, "content-type").is_some_and(|value| value.contains("application/x-www-form-urlencoded")));
    assert_eq!(header(&device, "accept").as_deref(), Some("application/json"));
    assert!(header(&device, "user-agent").is_some());
    let fields = form(&device);
    assert_eq!(form_param(&fields, "client_id").as_deref(), Some(CLIENT_ID));
    assert_eq!(
        form_param(&fields, "scope").as_deref(),
        Some("openid profile email offline_access grok-cli:access api:access")
    );
    assert_eq!(form_param(&fields, "referrer").as_deref(), Some("opencode"));

    let status = wait_for_status(&harness, &attempt).await;
    assert!(matches!(status, AttemptStatus::Complete { .. }), "got {status:?}");
    let after = opencode_util::time::now_ms();
    assert_eq!(polls.load(Ordering::SeqCst), 2);
    let token_polls: Vec<_> = routes.requests().into_iter().filter(|request| request.url == TOKEN_URL).collect();
    assert_eq!(token_polls.len(), 2);
    for request in &token_polls {
        let fields = form(request);
        assert_eq!(form_param(&fields, "grant_type").as_deref(), Some(DEVICE_GRANT));
        assert_eq!(form_param(&fields, "client_id").as_deref(), Some(CLIENT_ID));
        assert_eq!(form_param(&fields, "device_code").as_deref(), Some("device-code"));
    }

    let credentials = harness.services.credentials.list(&integration_id()).expect("list the xAI credentials");
    assert_eq!(credentials.len(), 1);
    let CredentialValue::Oauth(saved) = &credentials[0].value else { panic!("the xAI credential is OAuth") };
    assert_eq!(saved.method_id, method_id());
    assert_eq!(saved.access, "access");
    assert_eq!(saved.refresh, "refresh");
    assert!(saved.expires >= before as f64 + 3_600_000.0);
    assert!(saved.expires <= after as f64 + 3_600_000.0);
}

#[tokio::test(start_paused = true)]
async fn reports_a_denied_device_authorization() {
    let routes = Routes::new();
    routes.route_json(
        "POST",
        DEVICE_CODE_URL,
        json!({
            "device_code": "device-code",
            "user_code": "ABCD-EFGH",
            "verification_uri": "https://accounts.x.ai/device",
            "expires_in": 600,
            "interval": 5,
        }),
    );
    routes.route_with("POST", TOKEN_URL, |_| Reply::json_status(json!({ "error": "access_denied" }), 400));
    let mut harness = start(Input::new(Vec::new())).await;
    add_plugin(&mut harness, routes.clone()).await;

    let attempt = connect(&harness).await;
    assert_eq!(attempt.url, "https://accounts.x.ai/device");
    let status = wait_for_status(&harness, &attempt).await;
    let AttemptStatus::Failed { message, .. } = status else { panic!("the xAI attempt fails, got {status:?}") };
    assert_eq!(message, "xAI device authorization was denied");
    assert_eq!(harness.services.credentials.list(&integration_id()).expect("list the xAI credentials"), []);
}

#[tokio::test(start_paused = true)]
async fn refreshes_a_stored_device_credential() {
    let routes = Routes::new();
    routes.route_json(
        "POST",
        TOKEN_URL,
        json!({ "access_token": "new-access", "refresh_token": "new-refresh", "expires_in": 3600 }),
    );
    let mut harness = start(Input::new(Vec::new())).await;
    add_plugin(&mut harness, routes.clone()).await;
    let stored = harness
        .services
        .credentials
        .create(CreateInput {
            id: None,
            integration_id: integration_id(),
            value: oauth(DEVICE, "old-access", "old-refresh", 1.0),
            label: None,
            activate: None,
        })
        .expect("create the xAI credential");
    let connection = harness
        .services
        .integration
        .connection()
        .active(&integration_id())
        .await
        .expect("the active xAI connection")
        .expect("the xAI connection");

    let resolved = harness
        .services
        .integration
        .connection()
        .resolve(&connection)
        .await
        .expect("refresh the xAI credential")
        .expect("the xAI credential");
    let CredentialValue::Oauth(value) = resolved else { panic!("the xAI credential is OAuth") };
    assert_eq!(value.access, "new-access");
    assert_eq!(value.refresh, "new-refresh");

    let request = routes.requests().first().expect("the xAI refresh request").clone();
    assert_eq!(request.url, TOKEN_URL);
    let fields = form(&request);
    assert_eq!(form_param(&fields, "grant_type").as_deref(), Some("refresh_token"));
    assert_eq!(form_param(&fields, "refresh_token").as_deref(), Some("old-refresh"));
    assert_eq!(form_param(&fields, "client_id").as_deref(), Some(CLIENT_ID));

    let updated =
        harness.services.credentials.get(&stored.id).expect("get the xAI credential").expect("the credential");
    let CredentialValue::Oauth(updated) = updated.value else { panic!("the xAI credential is OAuth") };
    assert_eq!(updated.access, "new-access");
    assert_eq!(updated.refresh, "new-refresh");
    assert!(updated.expires > 0.0);
}
