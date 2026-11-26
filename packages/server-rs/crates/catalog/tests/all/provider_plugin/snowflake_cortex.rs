use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use opencode_ai::route::compile::{Compiled, StreamOptions, compile};
use opencode_ai::route::executor::PreparedHttp;
use opencode_ai::schema::{
    CacheHint, CacheHintType, ContentPart, GenerationOptions, LlmRequest, Message, TextPart, UserMessage,
};
use opencode_ai::{ExecutionContext, MapEnv};
use opencode_bus::{Definition, PublishOptions};
use opencode_catalog::builtin::Builtin;
use opencode_catalog::credential::{CreateInput, Info};
use opencode_catalog::integration::{Attempt, AttemptStatus, KeyInput, OAuthConnectInput};
use opencode_catalog::provider_plugin::hooks::{
    HttpRequest, HttpResponse, ProviderHooks, RequestKind, Retry, RetryDecision,
};
use opencode_catalog::provider_plugin::http::BufferedResponse;
use opencode_catalog::provider_plugin::snowflake_cortex;
use opencode_catalog::resolver::{self, Resolver};
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    AgentID, ConnectionEnvInfo, ConnectionEnvInfoType, ConnectionInfo, CredentialOAuth, CredentialOAuthType,
    CredentialValue, FormAnswer, FormValue, IntegrationID, IntegrationMethodID, ModelCacheFormat, ModelMaxTokensField,
    ModelRef, ProviderID, SessionID,
};
use serde_json::{Value, json};
use url::Url;

use crate::builtin::{Harness, Input, start};
use crate::provider_plugin::support::{Routes, base_url, plugin_context_with, provider_settings, wait_for_setting};

const PROVIDER_ID: &str = "snowflake-cortex";

const METHOD_ID: &str = "browser";

const MODEL_ID: &str = "claude-sonnet-4-6";

const ENDPOINT: &str = "https://myorg-myaccount.snowflakecomputing.com";

const TOKEN: &str = "https://myorg-myaccount.snowflakecomputing.com/oauth/token-request";

const OPENAI_COMPATIBLE: &str = "@opencode/ai/providers/openai-compatible";

const BASE_URL: &str = "https://${SNOWFLAKE_ACCOUNT}.snowflakecomputing.com/api/v2/cortex/v1";

const USER_AGENT: &str = "opencode/test/test/opencode";

const CONVERSATION_COMPLETE: &[u8] =
    b"data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";

fn provider_id() -> ProviderID {
    ProviderID(PROVIDER_ID.to_owned())
}

fn integration_id() -> IntegrationID {
    IntegrationID(PROVIDER_ID.to_owned())
}

fn method_id() -> IntegrationMethodID {
    IntegrationMethodID(METHOD_ID.to_owned())
}

fn client() -> reqwest::Client {
    opencode_config::http::client()
}

fn form_answer(pairs: &[(&str, &str)]) -> FormAnswer {
    JsonMap(pairs.iter().map(|(key, value)| ((*key).to_owned(), FormValue::String((*value).to_owned()))).collect())
}

fn metadata_account(account: &str) -> Option<JsonMap<Value>> {
    Some(JsonMap(vec![("account".to_owned(), Value::String(account.to_owned()))]))
}

async fn seed_provider(harness: &mut Harness) {
    let registration = harness
        .services
        .provider
        .transform(|editor| {
            let id = provider_id();
            editor.update(&id, |provider| {
                provider.package = OPENAI_COMPATIBLE.to_owned();
                provider.settings = Some(provider_settings(BASE_URL));
            });
            editor.models_update(&id, MODEL_ID, |_| {});
            Ok(())
        })
        .await;
    harness.registrations.registrations.push(registration);
}

async fn add_plugin(harness: &mut Harness, routes: Arc<Routes>) -> Arc<ProviderHooks> {
    let context = plugin_context_with(harness, routes);
    let hooks = context.hooks.clone();
    let mut builtin = Builtin::default();
    snowflake_cortex::register(&harness.services, &context, &mut builtin).await;
    harness.record(builtin);
    hooks
}

struct Test {
    harness: Harness,
    routes: Arc<Routes>,
    hooks: Arc<ProviderHooks>,
}

async fn setup(input: Input) -> Test {
    let mut harness = start(input).await;
    seed_provider(&mut harness).await;
    let routes = Routes::new();
    let hooks = add_plugin(&mut harness, routes.clone()).await;
    Test { harness, routes, hooks }
}

fn retry_event(attempt: u32, decision: RetryDecision) -> Retry {
    Retry {
        session_id: SessionID("ses_test".to_owned()),
        agent: AgentID("build".to_owned()),
        model: ModelRef { id: MODEL_ID.to_owned(), provider_id: provider_id(), variant: None },
        error: json!({ "type": "provider.authentication", "message": "Unauthorized", "status": 401 }),
        attempt,
        decision,
    }
}

fn prepared(headers: &[(&str, &str)]) -> PreparedHttp {
    PreparedHttp::new(
        "POST",
        format!("{ENDPOINT}/api/v2/cortex/v1/chat/completions"),
        headers.iter().map(|(name, value)| ((*name).to_owned(), (*value).to_owned())).collect(),
        Bytes::new(),
    )
}

fn http_request(headers: &[(&str, &str)]) -> HttpRequest {
    HttpRequest {
        session_id: SessionID("ses_test".to_owned()),
        agent: AgentID("build".to_owned()),
        model: ModelRef { id: MODEL_ID.to_owned(), provider_id: provider_id(), variant: None },
        kind: RequestKind::Primary,
        request: prepared(headers),
    }
}

fn http_response(status: u16, body: &[u8]) -> HttpResponse {
    HttpResponse {
        session_id: SessionID("ses_test".to_owned()),
        agent: AgentID("build".to_owned()),
        model: ModelRef { id: MODEL_ID.to_owned(), provider_id: provider_id(), variant: None },
        kind: RequestKind::Primary,
        request: prepared(&[]),
        response: BufferedResponse {
            status,
            headers: vec![("content-type".to_owned(), "application/json".to_owned())],
            body: Bytes::copy_from_slice(body),
        },
    }
}

async fn connect(harness: &Harness, answer: Option<FormAnswer>) -> Attempt {
    harness
        .services
        .integration
        .oauth()
        .connect(OAuthConnectInput { integration_id: integration_id(), method_id: method_id(), answer, label: None })
        .await
        .expect("connect the Snowflake integration")
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
                .expect("the Snowflake attempt status");
            if !matches!(status, AttemptStatus::Pending { .. }) {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the Snowflake attempt leaves pending")
}

fn credentials(harness: &Harness) -> Vec<Info> {
    harness.services.credentials.list(&integration_id()).expect("list the Snowflake credentials")
}

async fn active(harness: &Harness) -> Option<ConnectionInfo> {
    harness.services.integration.connection().active(&integration_id()).await.expect("the active connection")
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

fn form(request: &PreparedHttp) -> Vec<(String, String)> {
    url::form_urlencoded::parse(&request.body).map(|(key, value)| (key.into_owned(), value.into_owned())).collect()
}

fn form_param(fields: &[(String, String)], name: &str) -> Option<String> {
    fields.iter().find(|(key, _)| key == name).map(|(_, value)| value.clone())
}

fn header(request: &PreparedHttp, name: &str) -> Option<String> {
    request.headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.clone())
}

async fn compiled(harness: &Harness, request: LlmRequest) -> Compiled {
    let resolver = Resolver::new(
        harness.services.provider.clone(),
        harness.services.model.clone(),
        harness.services.integration.clone(),
        Arc::new(MapEnv::default()),
        Arc::new(resolver::NoBridge),
    );
    let reference = ModelRef { id: MODEL_ID.to_owned(), provider_id: provider_id(), variant: None };
    let resolved =
        resolver.resolve(Some(&reference)).await.expect("resolve the Snowflake model").expect("the Snowflake model");
    compile(&resolved.model, request, &StreamOptions::default(), &ExecutionContext::system())
        .expect("compile the Snowflake request")
}

fn hello() -> LlmRequest {
    LlmRequest {
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
    }
}

fn expired_oauth(account: &str, refresh: &str) -> CredentialValue {
    CredentialValue::Oauth(CredentialOAuth {
        r#type: CredentialOAuthType::Oauth,
        method_id: method_id(),
        access: "expired".to_owned(),
        refresh: refresh.to_owned(),
        expires: 1.0,
        metadata: metadata_account(account),
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browser_oauth_supplies_the_native_endpoint_and_token_and_refreshes_a_rejected_token_once() {
    let test = setup(Input::new(Vec::new())).await;
    let account = format!("{ENDPOINT}///");
    let attempt =
        connect(&test.harness, Some(form_answer(&[("account", account.as_str()), ("role", "My Role")]))).await;
    let url = Url::parse(&attempt.url).expect("the Snowflake authorization url");
    assert_eq!(format!("{}{}", url.origin().ascii_serialization(), url.path()), format!("{ENDPOINT}/oauth/authorize"));
    assert_eq!(param(&url, "scope").as_deref(), Some("refresh_token session:role-encoded:My%20Role"));
    assert_eq!(param(&url, "code_challenge_method").as_deref(), Some("S256"));
    assert!(param(&url, "state").is_some_and(|state| !state.is_empty()));
    let mut callback =
        Url::parse(&param(&url, "redirect_uri").expect("the Snowflake redirect")).expect("the Snowflake callback url");
    assert_eq!(callback.host_str(), Some("127.0.0.1"));
    assert_eq!(callback.path(), "/");
    set_param(&mut callback, "state", &param(&url, "state").expect("the Snowflake state"));
    set_param(&mut callback, "code", "auth-code");

    test.routes.route_json(
        "POST",
        TOKEN,
        json!({ "access_token": "access", "refresh_token": "refresh", "expires_in": 3600 }),
    );
    let response = client().get(callback.as_str()).send().await.expect("fetch the Snowflake callback");
    assert_eq!(response.status(), 200);
    let status = wait_for_status(&test.harness, &attempt).await;
    assert!(matches!(status, AttemptStatus::Complete { .. }), "got {status:?}");

    let exchange = test.routes.requests().first().expect("the Snowflake token exchange").clone();
    assert_eq!(exchange.url, TOKEN);
    assert_eq!(
        header(&exchange, "authorization").as_deref(),
        Some(format!("Basic {}", STANDARD.encode("LOCAL_APPLICATION:LOCAL_APPLICATION")).as_str())
    );
    let fields = form(&exchange);
    assert_eq!(form_param(&fields, "grant_type").as_deref(), Some("authorization_code"));
    assert_eq!(form_param(&fields, "code").as_deref(), Some("auth-code"));
    assert_eq!(form_param(&fields, "redirect_uri"), param(&url, "redirect_uri"));
    assert_eq!(form_param(&fields, "client_id").as_deref(), Some("LOCAL_APPLICATION"));
    let verifier = form_param(&fields, "code_verifier").expect("the Snowflake code verifier");
    assert_eq!(param(&url, "code_challenge").as_deref(), Some(challenge(&verifier).as_str()));

    let base = format!("{ENDPOINT}/api/v2/cortex/v1");
    wait_for_setting(&test.harness, PROVIDER_ID, "baseURL", &base).await;
    let initial = compiled(&test.harness, hello()).await;
    assert_eq!(initial.prepared.request.url, format!("{base}/chat/completions"));
    assert_eq!(header(&initial.prepared.request, "authorization").as_deref(), Some("Bearer access"));

    test.routes.set_json("POST", TOKEN, json!({ "access_token": "renewed", "refresh_token": "", "expires_in": 3600 }));
    let retry = test.hooks.trigger_retry(retry_event(2, RetryDecision::Retry { delay_ms: 1000 })).await;
    assert_eq!(retry.decision, RetryDecision::Retry { delay_ms: 0 });
    let refresh = test.routes.requests().last().expect("the Snowflake refresh request").clone();
    let refresh_fields = form(&refresh);
    assert_eq!(form_param(&refresh_fields, "grant_type").as_deref(), Some("refresh_token"));
    assert_eq!(form_param(&refresh_fields, "refresh_token").as_deref(), Some("refresh"));

    let saved = credentials(&test.harness);
    let CredentialValue::Oauth(value) = &saved[0].value else { panic!("the Snowflake credential is OAuth") };
    assert_eq!(value.access, "renewed");
    assert_eq!(value.refresh, "refresh");

    let renewed = compiled(&test.harness, hello()).await;
    assert_eq!(header(&renewed.prepared.request, "authorization").as_deref(), Some("Bearer renewed"));

    let event = test.hooks.trigger_retry(retry_event(3, RetryDecision::Retry { delay_ms: 1000 })).await;
    assert_eq!(event.decision, RetryDecision::Retry { delay_ms: 1000 });

    let body = br#"{"message":"Conversation complete","error":{}}"#;
    let event = test.hooks.trigger_http_response(http_response(400, body)).await;
    assert_eq!(event.response.status, 200);
    assert_eq!(event.response.headers, [("content-type".to_owned(), "text/event-stream".to_owned())]);
    assert_eq!(event.response.body.as_ref(), CONVERSATION_COMPLETE);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_pat_uses_its_account_and_native_compatibility() {
    let mut harness = start(Input::new(Vec::new())).await;
    seed_provider(&mut harness).await;
    let routes = Routes::new();
    let _hooks = add_plugin(&mut harness, routes.clone()).await;
    let account = format!("{ENDPOINT}/");
    harness
        .services
        .integration
        .connection()
        .key(KeyInput {
            integration_id: integration_id(),
            key: "pat".to_owned(),
            answer: Some(form_answer(&[("account", account.as_str())])),
            label: None,
        })
        .await
        .expect("create the Snowflake key credential");

    let base = format!("{ENDPOINT}/api/v2/cortex/v1");
    wait_for_setting(&harness, PROVIDER_ID, "baseURL", &base).await;

    let model = harness.services.model.get(&provider_id(), MODEL_ID).await.unwrap().expect("the Snowflake model");
    let compatibility = model.compatibility.expect("the Snowflake compatibility");
    assert_eq!(compatibility.max_tokens_field, Some(ModelMaxTokensField::MaxCompletionTokens));
    assert_eq!(compatibility.cache_format, Some(ModelCacheFormat::Anthropic));

    let request = LlmRequest {
        messages: vec![Message::User(UserMessage {
            id: None,
            content: vec![ContentPart::Text(TextPart {
                text: "Hello".to_owned(),
                cache: Some(CacheHint { r#type: CacheHintType::Ephemeral, ttl: None }),
                provider_metadata: None,
            })],
            provider_metadata: None,
        })],
        generation: GenerationOptions { max_tokens: Some(serde_json::Number::from(1024)), ..Default::default() },
        ..LlmRequest::default()
    };
    let compiled = compiled(&harness, request).await;
    assert_eq!(compiled.prepared.request.url, format!("{base}/chat/completions"));
    assert_eq!(header(&compiled.prepared.request, "authorization").as_deref(), Some("Bearer pat"));
    assert_eq!(compiled.body.get("max_completion_tokens").and_then(Value::as_f64), Some(1024.0));
    assert!(compiled.body.get("max_tokens").is_none());
    assert!(serde_json::to_string(&compiled.body).unwrap().contains("\"cache_control\":{\"type\":\"ephemeral\"}"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ignores_a_mismatched_callback_state_and_still_completes_the_login() {
    let test = setup(Input::new(Vec::new())).await;
    let attempt = connect(&test.harness, Some(form_answer(&[("account", "myorg-myaccount")]))).await;
    let url = Url::parse(&attempt.url).expect("the Snowflake authorization url");
    let redirect = param(&url, "redirect_uri").expect("the Snowflake redirect");
    let mut stray = Url::parse(&redirect).expect("the Snowflake callback url");
    set_param(&mut stray, "state", "wrong");
    set_param(&mut stray, "code", "forged");
    let response = client().get(stray.as_str()).send().await.expect("fetch the Snowflake callback");
    assert_eq!(response.status(), 400);
    let status = test
        .harness
        .services
        .integration
        .oauth()
        .status(&integration_id(), &attempt.attempt_id)
        .await
        .expect("the Snowflake attempt status");
    assert!(matches!(status, AttemptStatus::Pending { .. }));

    test.routes.route_json(
        "POST",
        TOKEN,
        json!({ "access_token": "access", "refresh_token": "refresh", "expires_in": 3600 }),
    );
    let mut callback = Url::parse(&redirect).expect("the Snowflake callback url");
    set_param(&mut callback, "state", &param(&url, "state").expect("the Snowflake state"));
    set_param(&mut callback, "code", "auth-code");
    let response = client().get(callback.as_str()).send().await.expect("fetch the Snowflake callback");
    assert_eq!(response.status(), 200);
    let status = wait_for_status(&test.harness, &attempt).await;
    assert!(matches!(status, AttemptStatus::Complete { .. }), "got {status:?}");
    let exchange = test.routes.requests().first().expect("the Snowflake token exchange").clone();
    assert_eq!(form_param(&form(&exchange), "code").as_deref(), Some("auth-code"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uses_environment_tokens_rather_than_the_account_identifier_and_bypasses_expired_stored_oauth() {
    let account = format!("{ENDPOINT}/");
    let test = setup(
        Input::new(Vec::new())
            .environment("SNOWFLAKE_ACCOUNT", &account)
            .environment("SNOWFLAKE_CORTEX_TOKEN", "env-token")
            .environment("SNOWFLAKE_CORTEX_PAT", "env-pat"),
    )
    .await;
    assert_eq!(
        active(&test.harness).await,
        Some(ConnectionInfo::Env(ConnectionEnvInfo {
            r#type: ConnectionEnvInfoType::Env,
            name: "SNOWFLAKE_CORTEX_TOKEN".to_owned(),
            status: None,
        }))
    );
    let provider = test.harness.services.provider.get(&provider_id()).await.unwrap().expect("the Snowflake provider");
    assert_eq!(provider.package, OPENAI_COMPATIBLE);
    assert_eq!(base_url(&provider), Some(format!("{ENDPOINT}/api/v2/cortex/v1")));

    let event = test.hooks.trigger_http_request(http_request(&[("authorization", "Bearer stale")])).await;
    assert_eq!(header(&event.request, "authorization").as_deref(), Some("Bearer env-token"));
    assert_eq!(header(&event.request, "user-agent").as_deref(), Some(USER_AGENT));

    let saved = test
        .harness
        .services
        .credentials
        .create(CreateInput {
            id: None,
            integration_id: integration_id(),
            value: expired_oauth("other-account", "revoked-refresh"),
            label: None,
            activate: None,
        })
        .expect("create the expired Snowflake credential");
    test.harness
        .services
        .bus
        .publish(
            &Definition::ephemeral("credential.switched"),
            json!({ "integrationID": PROVIDER_ID, "credentialID": saved.id.0 }),
            PublishOptions { global: true, ..Default::default() },
        )
        .expect("publish the Snowflake credential switch");
    tokio::time::sleep(Duration::from_millis(50)).await;

    let provider = test.harness.services.provider.get(&provider_id()).await.unwrap().expect("the Snowflake provider");
    assert_eq!(base_url(&provider), Some(format!("{ENDPOINT}/api/v2/cortex/v1")));
    let event = test.hooks.trigger_http_request(http_request(&[])).await;
    assert_eq!(header(&event.request, "authorization").as_deref(), Some("Bearer env-token"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn requires_a_token_separately_from_the_account_and_supports_environment_pats() {
    let account = format!("{ENDPOINT}/");
    let test = setup(Input::new(Vec::new()).environment("SNOWFLAKE_ACCOUNT", &account)).await;
    assert_eq!(active(&test.harness).await, None);

    let test = setup(
        Input::new(Vec::new())
            .environment("SNOWFLAKE_ACCOUNT", &account)
            .environment("SNOWFLAKE_CORTEX_PAT", "env-pat"),
    )
    .await;
    let event = test.hooks.trigger_http_request(http_request(&[])).await;
    assert_eq!(header(&event.request, "authorization").as_deref(), Some("Bearer env-pat"));
    let body = json!({ "message": "Invalid model" }).to_string();
    let event = test.hooks.trigger_http_response(http_response(400, body.as_bytes())).await;
    assert_eq!(event.response.status, 400);
    assert_eq!(event.response.body.as_ref(), body.as_bytes());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refreshes_expired_oauth_during_native_model_resolution_and_persists_rotated_credentials() {
    let mut harness = start(Input::new(Vec::new())).await;
    seed_provider(&mut harness).await;
    let saved = harness
        .services
        .credentials
        .create(CreateInput {
            id: None,
            integration_id: integration_id(),
            value: CredentialValue::Oauth(CredentialOAuth {
                r#type: CredentialOAuthType::Oauth,
                method_id: method_id(),
                access: "expired".to_owned(),
                refresh: "refresh".to_owned(),
                expires: 1.0,
                metadata: metadata_account("myorg-myaccount"),
            }),
            label: None,
            activate: None,
        })
        .expect("create the expired Snowflake credential");
    let routes = Routes::new();
    let _hooks = add_plugin(&mut harness, routes.clone()).await;
    routes.route_json(
        "POST",
        TOKEN,
        json!({ "access_token": "renewed", "refresh_token": "rotated-refresh", "expires_in": 3600 }),
    );

    let connection = active(&harness).await.expect("the Snowflake connection");
    let resolved = harness
        .services
        .integration
        .connection()
        .resolve(&connection)
        .await
        .expect("refresh the Snowflake credential")
        .expect("the Snowflake credential");
    let request = routes.requests().first().expect("the Snowflake refresh request").clone();
    assert_eq!(request.url, TOKEN);
    assert_eq!(
        form(&request),
        [
            ("grant_type".to_owned(), "refresh_token".to_owned()),
            ("refresh_token".to_owned(), "refresh".to_owned()),
            ("client_id".to_owned(), "LOCAL_APPLICATION".to_owned()),
        ]
    );
    let stored = harness.services.credentials.get(&saved.id).unwrap().expect("the Snowflake credential");
    let CredentialValue::Oauth(value) = &stored.value else { panic!("the Snowflake credential is OAuth") };
    assert_eq!(value.access, "renewed");
    assert_eq!(value.refresh, "rotated-refresh");
    assert_eq!(
        value
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.0.iter().find(|(key, _)| key == "account"))
            .map(|(_, value)| value.clone()),
        Some(json!("myorg-myaccount"))
    );
    assert!(matches!(resolved, CredentialValue::Oauth(_)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_retries_share_one_token_refresh() {
    let test = setup(Input::new(Vec::new())).await;
    test.harness
        .services
        .credentials
        .create(CreateInput {
            id: None,
            integration_id: integration_id(),
            value: expired_oauth(ENDPOINT, "refresh"),
            label: None,
            activate: Some(true),
        })
        .expect("create the expired Snowflake credential");
    test.routes.route_json(
        "POST",
        TOKEN,
        json!({ "access_token": "renewed", "refresh_token": "", "expires_in": 3600 }),
    );
    test.routes.hold("POST", TOKEN);
    let before = test.routes.count("POST", TOKEN);
    let spawn = || {
        let hooks = test.hooks.clone();
        tokio::spawn(async move { hooks.trigger_retry(retry_event(2, RetryDecision::Retry { delay_ms: 1000 })).await })
    };
    let first = spawn();
    let second = spawn();
    test.routes.arrived().await;
    tokio::time::sleep(Duration::from_millis(25)).await;
    test.routes.release();
    test.routes.release();

    let first = first.await.expect("the first Snowflake retry");
    let second = second.await.expect("the second Snowflake retry");
    assert_eq!(first.decision, RetryDecision::Retry { delay_ms: 0 });
    assert_eq!(second.decision, RetryDecision::Retry { delay_ms: 0 });
    assert_eq!(test.routes.count("POST", TOKEN) - before, 1);
}
