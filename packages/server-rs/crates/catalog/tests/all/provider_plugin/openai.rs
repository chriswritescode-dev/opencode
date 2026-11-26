use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use indexmap::IndexMap;
use opencode_catalog::builtin::Builtin;
use opencode_catalog::credential::CreateInput;
use opencode_catalog::integration::{Attempt, AttemptMode, AttemptStatus, OAuthConnectInput};
use opencode_catalog::provider_plugin::hooks::{Compaction, Context, ModelRequest, RequestKind};
use opencode_catalog::provider_plugin::openai;
use opencode_catalog::provider_plugin::{PluginContext, SessionRef};
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    AgentID, CredentialKey, CredentialKeyType, CredentialOAuth, CredentialOAuthType, CredentialValue, IntegrationID,
    IntegrationMethod, IntegrationMethodID, ModelCost, ModelCostCache, ModelInfo, ModelInfoLimit, ModelRef, ModelType,
    ModelTypeString3, MoneyUSDPerMillionTokens, ProviderEncoding, ProviderID, ProviderTransport, SessionID,
};
use serde_json::{Value, json};
use url::Url;

use crate::builtin::{Harness, Input, start};
use crate::provider_plugin::support::{FakeSessions, Reply, Routes, base_url, headers, plugin_context_with};

const INTEGRATION_ID: &str = "openai";

const PACKAGE: &str = "@opencode/ai/providers/openai";

const CODEX_URL: &str = "https://chatgpt.com/backend-api/codex";

const BROWSER: &str = "chatgpt-browser";

const HEADLESS: &str = "chatgpt-headless";

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

const ISSUER: &str = "https://auth.openai.com";

const USERCODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";

const DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";

const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";

const DEVICE_REDIRECT: &str = "https://auth.openai.com/deviceauth/callback";

const SCOPE: &str = "openid profile email offline_access";

const FULL: ModelInfoLimit = ModelInfoLimit { context: 1_050_000.0, input: Some(922_000.0), output: 128_000.0 };

fn integration_id() -> IntegrationID {
    IntegrationID(INTEGRATION_ID.to_owned())
}

fn provider_id() -> ProviderID {
    ProviderID(INTEGRATION_ID.to_owned())
}

fn method_id(method: &str) -> IntegrationMethodID {
    IntegrationMethodID(method.to_owned())
}

fn oauth(method: &str, access: &str, refresh: &str, account: Option<&str>) -> CredentialValue {
    CredentialValue::Oauth(CredentialOAuth {
        r#type: CredentialOAuthType::Oauth,
        method_id: method_id(method),
        access: access.to_owned(),
        refresh: refresh.to_owned(),
        expires: 9_999_999_999_999.0,
        metadata: account.map(|account| JsonMap(vec![("accountID".to_owned(), json!(account))])),
    })
}

fn key(key: &str) -> CredentialValue {
    CredentialValue::Key(CredentialKey { r#type: CredentialKeyType::Key, key: key.to_owned(), metadata: None })
}

fn cost(input: f64, output: f64) -> ModelCost {
    ModelCost {
        tier: None,
        input: MoneyUSDPerMillionTokens(input),
        output: MoneyUSDPerMillionTokens(output),
        cache: ModelCostCache { read: MoneyUSDPerMillionTokens(0.0), write: MoneyUSDPerMillionTokens(0.0) },
    }
}

async fn register(harness: &mut Harness, context: &PluginContext) {
    let mut builtin = Builtin::default();
    openai::register(&harness.services, context, &mut builtin).await;
    harness.record(builtin);
}

async fn add_plugin(harness: &mut Harness, routes: Arc<Routes>) -> PluginContext {
    let context = plugin_context_with(harness, routes);
    register(harness, &context).await;
    context
}

async fn request(context: &PluginContext, provider: &str, base_url: &str, session: &str) -> ModelRequest {
    context
        .hooks
        .trigger_model_request(ModelRequest {
            session_id: SessionID(session.to_owned()),
            agent: AgentID("build".to_owned()),
            model: ModelRef { id: "gpt-5.5".to_owned(), provider_id: ProviderID(provider.to_owned()), variant: None },
            kind: RequestKind::Primary,
            base_url: Some(base_url.to_owned()),
            headers: IndexMap::new(),
        })
        .await
}

fn model_headers(model: &ModelInfo) -> HashMap<String, String> {
    model.headers.as_ref().map(|headers| headers.0.iter().cloned().collect()).unwrap_or_default()
}

fn model_settings(model: &ModelInfo) -> Value {
    model
        .settings
        .as_ref()
        .map(|settings| {
            serde_json::from_str(&opencode_contract::codec::encode_json(settings)).expect("the model settings json")
        })
        .unwrap_or(Value::Null)
}

async fn model(harness: &Harness, id: &str) -> ModelInfo {
    harness
        .services
        .model
        .get(&provider_id(), id)
        .await
        .expect("the OpenAI model lookup")
        .unwrap_or_else(|| panic!("the {id} model"))
}

async fn provider(harness: &Harness) -> opencode_contract::generated::ProviderInfo {
    harness
        .services
        .provider
        .get(&provider_id())
        .await
        .expect("the OpenAI provider lookup")
        .expect("the OpenAI provider")
}

async fn connect(harness: &Harness, method: &str) -> Attempt {
    harness
        .services
        .integration
        .oauth()
        .connect(OAuthConnectInput {
            integration_id: integration_id(),
            method_id: method_id(method),
            answer: None,
            label: None,
        })
        .await
        .expect("connect the OpenAI integration")
}

async fn wait_for_status_advancing(harness: &Harness, attempt: &Attempt) -> AttemptStatus {
    for _ in 0..100 {
        let status = harness
            .services
            .integration
            .oauth()
            .status(&integration_id(), &attempt.attempt_id)
            .await
            .expect("the OpenAI attempt status");
        if !matches!(status, AttemptStatus::Pending { .. }) {
            return status;
        }
        tokio::time::advance(Duration::from_secs(10)).await;
        tokio::task::yield_now().await;
    }
    panic!("the OpenAI attempt leaves pending")
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
                .expect("the OpenAI attempt status");
            if !matches!(status, AttemptStatus::Pending { .. }) {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the OpenAI attempt leaves pending")
}

async fn wait_for_header(harness: &Harness, name: &str, value: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let provider_headers = headers(&provider(harness).await);
            if provider_headers
                .is_some_and(|provider_headers| provider_headers.get(name).is_some_and(|current| current == value))
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the provider header settles")
}

fn form(request: &opencode_ai::route::executor::PreparedHttp) -> Vec<(String, String)> {
    url::form_urlencoded::parse(&request.body).map(|(key, value)| (key.into_owned(), value.into_owned())).collect()
}

fn form_param(fields: &[(String, String)], name: &str) -> Option<String> {
    fields.iter().find(|(key, _)| key == name).map(|(_, value)| value.clone())
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

fn id_token(account: &str) -> String {
    let claims = json!({ "https://api.openai.com/auth": { "chatgpt_account_id": account } });
    format!("e30.{}.sig", URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).expect("the id token claims")))
}

fn options() -> IndexMap<String, Value> {
    IndexMap::from([("maxTokens".to_owned(), json!(128_000))])
}

fn context_event(provider: &str) -> Context {
    Context {
        session_id: SessionID("ses_test".to_owned()),
        agent: AgentID("build".to_owned()),
        model: ModelRef { id: "gpt-5.5".to_owned(), provider_id: ProviderID(provider.to_owned()), variant: None },
        kind: RequestKind::Primary,
        options: options(),
    }
}

fn compaction_event(provider: &str) -> Compaction {
    Compaction {
        session_id: SessionID("ses_test".to_owned()),
        agent: AgentID("build".to_owned()),
        model: ModelRef { id: "gpt-5.5".to_owned(), provider_id: ProviderID(provider.to_owned()), variant: None },
        kind: RequestKind::Primary,
        options: options(),
        result: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registers_browser_and_headless_chatgpt_oauth_methods() {
    let mut harness = start(Input::new(Vec::new())).await;
    add_plugin(&mut harness, Routes::new()).await;
    let integration = harness
        .services
        .integration
        .get(&integration_id())
        .await
        .expect("get the OpenAI integration")
        .expect("the OpenAI integration");
    assert_eq!(integration.methods.len(), 2);
    let IntegrationMethod::Oauth(browser) = &integration.methods[0] else {
        panic!("the browser OAuth method is first")
    };
    assert_eq!(browser.id, method_id(BROWSER));
    assert_eq!(browser.label, "Codex browser (legacy)");
    let IntegrationMethod::Oauth(headless) = &integration.methods[1] else {
        panic!("the headless OAuth method is second")
    };
    assert_eq!(headless.id, method_id(HEADLESS));
    assert_eq!(headless.label, "Codex device code (legacy)");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn filters_the_openai_catalog_to_codex_eligible_models_under_a_chatgpt_connection() {
    let routes = Routes::new();
    let mut harness = start(Input::new(Vec::new())).await;
    let seeded = harness
        .services
        .provider
        .transform(|editor| {
            let provider = provider_id();
            editor.update(&provider, |provider| provider.package = PACKAGE.to_owned());
            for id in [
                "gpt-5.5",
                "gpt-5.3-codex-spark",
                "gpt-5.5-pro",
                "gpt-5.6",
                "gpt-4.1",
                "gpt-5",
                "gpt-5.04-astra",
                "gpt-4.99",
            ] {
                editor.models_update(&provider, id, |_| {});
            }
            editor.models_update(&provider, "gpt-5.6-sol", |model| {
                model.limit = FULL;
                model.cost = vec![cost(1.0, 2.0)];
            });
            editor.models_update(&provider, "gpt-5.4", |model| {
                model.limit = ModelInfoLimit { context: 1_050_000.0, input: Some(922_000.0), output: 64_000.0 };
            });
            editor.models_update(&provider, "gpt-5.4-pro", |model| {
                model.model_id = "gpt-5.4".to_owned();
                model.body = Some(JsonMap(vec![("reasoning".to_owned(), json!({ "mode": "pro" }))]));
            });
            for id in ["gpt-6-astra", "gpt-5.10"] {
                editor.models_update(&provider, id, |model| model.limit = FULL);
            }
            editor.models_update(&provider, "gpt-image-1.5", |model| {
                model.r#type = Some(ModelType::String3(ModelTypeString3::Image));
            });
            editor.models_update(&provider, "gpt-image-2", |model| {
                model.r#type = Some(ModelType::String3(ModelTypeString3::Image));
                model.cost = vec![cost(8.0, 30.0)];
            });
            Ok(())
        })
        .await;
    harness.registrations.registrations.push(seeded);
    harness.credential(INTEGRATION_ID, oauth(BROWSER, "chatgpt-token", "refresh", Some("acct_123"))).await;

    let fake = FakeSessions::default();
    fake.insert(SessionRef {
        id: SessionID("ses_child".to_owned()),
        parent_id: Some(SessionID("ses_parent".to_owned())),
        fork_session_id: None,
    });
    let mut context = plugin_context_with(&harness, routes);
    context.sessions = Arc::new(fake);
    register(&mut harness, &context).await;

    let direct = request(&context, INTEGRATION_ID, "https://api.openai.com/v1", "ses_test").await;
    let custom = request(&context, "custom-openai", "https://api.openai.com/v1", "ses_test").await;
    let proxy = request(&context, INTEGRATION_ID, "https://proxy.example/v1?region=us", "ses_test").await;
    let child = request(&context, INTEGRATION_ID, "https://api.openai.com/v1", "ses_child").await;

    let info = provider(&harness).await;
    assert_eq!(info.package, PACKAGE);
    assert_eq!(base_url(&info).as_deref(), Some(CODEX_URL));
    let settings = info.settings.as_ref().expect("the OpenAI provider settings");
    assert_eq!(settings.transport, Some(ProviderTransport::Websocket));
    assert_eq!(settings.encoding, Some(ProviderEncoding::Zstd));
    let provider_headers = headers(&info).expect("the OpenAI provider headers");
    assert_eq!(provider_headers.get("originator").map(String::as_str), Some("opencode"));
    assert_eq!(provider_headers.get("chatgpt-account-id").map(String::as_str), Some("acct_123"));
    assert_eq!(provider_headers.get("x-codex-beta-features").map(String::as_str), Some("remote_compaction_v2"));

    assert_eq!(direct.base_url.as_deref(), Some(CODEX_URL));
    assert_eq!(direct.headers.get("originator").map(String::as_str), Some("opencode"));
    assert_eq!(direct.headers.get("session-id").map(String::as_str), Some("ses_test"));
    assert!(custom.headers.get("originator").is_none());
    assert_eq!(custom.base_url.as_deref(), Some("https://api.openai.com/v1"));
    assert_eq!(proxy.base_url.as_deref(), Some("https://proxy.example/v1?region=us"));
    assert_eq!(proxy.headers.get("originator").map(String::as_str), Some("opencode"));
    assert_eq!(proxy.headers.get("session-id").map(String::as_str), Some("ses_test"));
    assert_eq!(child.headers.get("session-id").map(String::as_str), Some("ses_parent"));

    let eligible = model(&harness, "gpt-5.6-sol").await;
    assert_eq!(eligible.package.as_deref(), Some(PACKAGE));
    let eligible_headers = model_headers(&eligible);
    assert_eq!(eligible_headers.get("originator").map(String::as_str), Some("opencode"));
    assert_eq!(eligible_headers.get("chatgpt-account-id").map(String::as_str), Some("acct_123"));
    assert!(eligible.cost.is_empty());
    assert_eq!(eligible.limit, FULL);
    assert!(eligible.enabled);

    for id in [
        "gpt-5.5",
        "gpt-5.3-codex-spark",
        "gpt-5.5-pro",
        "gpt-5.4-pro",
        "gpt-5.4",
        "gpt-5.6",
        "gpt-4.1",
        "gpt-5",
        "gpt-5.04-astra",
        "gpt-4.99",
    ] {
        assert!(!model(&harness, id).await.enabled, "{id}");
    }
    for id in ["gpt-6-astra", "gpt-5.10"] {
        assert!(model(&harness, id).await.enabled, "{id}");
    }

    let images = harness
        .services
        .model
        .available(Some(&[ModelType::String3(ModelTypeString3::Image)]))
        .await
        .expect("the available OpenAI image models");
    let images: Vec<ModelInfo> = images.into_iter().filter(|model| model.provider_id == provider_id()).collect();
    assert_eq!(images.iter().map(|model| model.id.as_str()).collect::<Vec<_>>(), ["gpt-image-2"]);
    assert!(images[0].cost.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keeps_the_full_openai_catalog_under_an_api_key_connection() {
    let mut harness = start(Input::new(Vec::new())).await;
    let seeded = harness
        .services
        .provider
        .transform(|editor| {
            let provider = provider_id();
            editor.update(&provider, |provider| provider.package = PACKAGE.to_owned());
            editor.models_update(&provider, "gpt-5.5", |model| model.limit = FULL);
            editor.models_update(&provider, "gpt-4.1", |_| {});
            Ok(())
        })
        .await;
    harness.registrations.registrations.push(seeded);
    harness.credential(INTEGRATION_ID, key("sk-test")).await;

    let context = add_plugin(&mut harness, Routes::new()).await;
    let direct = request(&context, INTEGRATION_ID, "https://api.openai.com/v1", "ses_test").await;

    let info = provider(&harness).await;
    let info_settings = info.settings.as_ref().expect("the OpenAI provider settings");
    assert_eq!(info_settings.transport, Some(ProviderTransport::Websocket));
    assert_eq!(info_settings.encoding, None);
    let provider_headers = headers(&info).unwrap_or_default();
    assert!(!provider_headers.contains_key("x-codex-beta-features"));
    assert!(!provider_headers.contains_key("originator"));

    let full = model(&harness, "gpt-5.5").await;
    assert_eq!(full.package.as_deref(), Some(PACKAGE));
    assert!(full.enabled);
    assert_eq!(full.limit, FULL);
    assert!(model_settings(&full).get("transport").is_none());
    assert!(direct.headers.get("originator").is_none());
    assert_eq!(direct.base_url.as_deref(), Some("https://api.openai.com/v1"));
    assert!(model(&harness, "gpt-4.1").await.enabled);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reloads_providers_on_a_switch_only_when_the_chatgpt_login_changes() {
    let mut harness = start(Input::new(Vec::new())).await;
    let seeded = harness
        .services
        .provider
        .transform(|editor| {
            editor.update(&provider_id(), |_| {});
            Ok(())
        })
        .await;
    harness.registrations.registrations.push(seeded);
    let first = harness
        .services
        .credentials
        .create(CreateInput {
            id: None,
            integration_id: integration_id(),
            value: key("sk-first"),
            label: None,
            activate: None,
        })
        .expect("create the first OpenAI key");
    add_plugin(&mut harness, Routes::new()).await;

    let folds = Arc::new(AtomicUsize::new(0));
    let _counting = harness
        .services
        .provider
        .transform({
            let folds = folds.clone();
            move |_| {
                folds.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        })
        .await;
    let before = folds.load(Ordering::SeqCst);
    assert_eq!(before, 1);

    harness.credential(INTEGRATION_ID, key("sk-second")).await;
    harness.services.credentials.activate(&first.id).expect("activate the first OpenAI key");
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(folds.load(Ordering::SeqCst), before);

    harness.credential(INTEGRATION_ID, oauth(BROWSER, "chatgpt-token", "refresh", Some("acct_123"))).await;
    wait_for_header(&harness, "chatgpt-account-id", "acct_123").await;
    assert!(folds.load(Ordering::SeqCst) > before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn omits_the_default_output_limit_from_openai_steps_and_compaction() {
    let mut harness = start(Input::new(Vec::new())).await;
    let context = add_plugin(&mut harness, Routes::new()).await;

    let openai_context = context.hooks.trigger_context(context_event(INTEGRATION_ID)).await;
    assert!(!openai_context.options.contains_key("maxTokens"));
    let openai_compaction = context.hooks.trigger_compaction(compaction_event(INTEGRATION_ID)).await;
    assert!(!openai_compaction.options.contains_key("maxTokens"));

    let azure_context = context.hooks.trigger_context(context_event("azure")).await;
    assert_eq!(azure_context.options.get("maxTokens"), Some(&json!(128_000)));
    let azure_compaction = context.hooks.trigger_compaction(compaction_event("azure")).await;
    assert_eq!(azure_compaction.options.get("maxTokens"), Some(&json!(128_000)));
}

#[tokio::test(start_paused = true)]
async fn completes_the_headless_device_authorization() {
    let routes = Routes::new();
    routes.route_json(
        "POST",
        USERCODE_URL,
        json!({ "device_auth_id": "device", "user_code": "ABCD-EFGH", "interval": "5" }),
    );
    let polls = Arc::new(AtomicUsize::new(0));
    routes.route_with("POST", DEVICE_TOKEN_URL, {
        let polls = polls.clone();
        move |_| {
            if polls.fetch_add(1, Ordering::SeqCst) == 0 {
                return Reply::status(403);
            }
            Reply::json(json!({ "authorization_code": "code", "code_verifier": "verifier" }))
        }
    });
    routes.route_json(
        "POST",
        TOKEN_URL,
        json!({
            "id_token": id_token("acct_device"),
            "access_token": "access",
            "refresh_token": "refresh",
            "expires_in": 3600,
        }),
    );
    let mut harness = start(Input::new(Vec::new())).await;
    let _context = add_plugin(&mut harness, routes.clone()).await;

    let attempt = connect(&harness, HEADLESS).await;
    assert_eq!(attempt.url, format!("{ISSUER}/codex/device"));
    assert_eq!(attempt.instructions, "Enter code: ABCD-EFGH");
    assert_eq!(attempt.mode, AttemptMode::Auto);
    assert!(!attempt.paste);

    let status = wait_for_status_advancing(&harness, &attempt).await;
    assert!(matches!(status, AttemptStatus::Complete { .. }), "got {status:?}");
    assert_eq!(polls.load(Ordering::SeqCst), 2);

    let saved = harness.services.credentials.list(&integration_id()).expect("list the OpenAI credentials");
    assert_eq!(saved.len(), 1);
    let CredentialValue::Oauth(saved) = &saved[0].value else { panic!("the OpenAI credential is OAuth") };
    assert_eq!(saved.method_id, method_id(HEADLESS));
    assert_eq!(saved.access, "access");
    assert_eq!(saved.refresh, "refresh");
    let metadata = saved.metadata.as_ref().expect("the OpenAI credential metadata");
    assert_eq!(
        metadata.0.iter().find(|(key, _)| key == "accountID").map(|(_, value)| value),
        Some(&json!("acct_device"))
    );

    let exchange =
        routes.requests().into_iter().find(|request| request.url == TOKEN_URL).expect("the OpenAI token exchange");
    let fields = form(&exchange);
    assert_eq!(form_param(&fields, "grant_type").as_deref(), Some("authorization_code"));
    assert_eq!(form_param(&fields, "code").as_deref(), Some("code"));
    assert_eq!(form_param(&fields, "redirect_uri").as_deref(), Some(DEVICE_REDIRECT));
    assert_eq!(form_param(&fields, "client_id").as_deref(), Some(CLIENT_ID));
    assert_eq!(form_param(&fields, "code_verifier").as_deref(), Some("verifier"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exchanges_a_browser_code_for_a_chatgpt_credential() {
    let routes = Routes::new();
    let mut harness = start(Input::new(Vec::new())).await;
    let _context = add_plugin(&mut harness, routes.clone()).await;

    let attempt = connect(&harness, BROWSER).await;
    assert_eq!(attempt.mode, AttemptMode::Auto);
    assert!(attempt.paste);
    assert_eq!(attempt.instructions, "Complete authorization in your browser. This window will close automatically.");

    let authorize = Url::parse(&attempt.url).expect("the OpenAI authorization url");
    assert_eq!(authorize.origin().ascii_serialization(), ISSUER);
    assert_eq!(authorize.path(), "/oauth/authorize");
    assert_eq!(param(&authorize, "response_type").as_deref(), Some("code"));
    assert_eq!(param(&authorize, "client_id").as_deref(), Some(CLIENT_ID));
    assert_eq!(param(&authorize, "scope").as_deref(), Some(SCOPE));
    assert_eq!(param(&authorize, "code_challenge_method").as_deref(), Some("S256"));
    assert_eq!(param(&authorize, "id_token_add_organizations").as_deref(), Some("true"));
    assert_eq!(param(&authorize, "codex_cli_simplified_flow").as_deref(), Some("true"));
    assert_eq!(param(&authorize, "originator").as_deref(), Some("opencode"));
    assert!(param(&authorize, "state").is_some_and(|state| !state.is_empty()));

    let redirect = param(&authorize, "redirect_uri").expect("the OpenAI redirect");
    let redirect_url = Url::parse(&redirect).expect("the OpenAI redirect url");
    assert_eq!(redirect_url.scheme(), "http");
    assert_eq!(redirect_url.host_str(), Some("localhost"));
    assert_eq!(redirect_url.path(), "/auth/callback");
    assert!(redirect_url.port().is_some_and(|port| port != 0));

    let mut callback = redirect_url.clone();
    set_param(&mut callback, "state", &param(&authorize, "state").expect("the OpenAI state"));
    set_param(&mut callback, "code", "auth-code");
    routes.route_json(
        "POST",
        TOKEN_URL,
        json!({
            "id_token": id_token("acct_browser"),
            "access_token": "access-browser",
            "refresh_token": "refresh-browser",
            "expires_in": 3600,
        }),
    );

    let response =
        opencode_config::http::client().get(callback.as_str()).send().await.expect("fetch the OpenAI callback");
    assert_eq!(response.status(), 200);
    assert!(response.text().await.expect("the OpenAI callback page").contains("Authorization successful"));

    let status = wait_for_status(&harness, &attempt).await;
    assert!(matches!(status, AttemptStatus::Complete { .. }), "got {status:?}");

    let saved = harness.services.credentials.list(&integration_id()).expect("list the OpenAI credentials");
    assert_eq!(saved.len(), 1);
    let CredentialValue::Oauth(saved) = &saved[0].value else { panic!("the OpenAI credential is OAuth") };
    assert_eq!(saved.method_id, method_id(BROWSER));
    assert_eq!(saved.access, "access-browser");
    assert_eq!(saved.refresh, "refresh-browser");
    let metadata = saved.metadata.as_ref().expect("the OpenAI credential metadata");
    assert_eq!(
        metadata.0.iter().find(|(key, _)| key == "accountID").map(|(_, value)| value),
        Some(&json!("acct_browser"))
    );

    let exchange =
        routes.requests().into_iter().find(|request| request.url == TOKEN_URL).expect("the OpenAI token exchange");
    let fields = form(&exchange);
    assert_eq!(form_param(&fields, "grant_type").as_deref(), Some("authorization_code"));
    assert_eq!(form_param(&fields, "code").as_deref(), Some("auth-code"));
    assert_eq!(form_param(&fields, "redirect_uri").as_deref(), Some(redirect.as_str()));
    assert_eq!(form_param(&fields, "client_id").as_deref(), Some(CLIENT_ID));
    let verifier = form_param(&fields, "code_verifier").expect("the OpenAI code verifier");
    assert_eq!(param(&authorize, "code_challenge").as_deref(), Some(challenge(&verifier).as_str()));
}
