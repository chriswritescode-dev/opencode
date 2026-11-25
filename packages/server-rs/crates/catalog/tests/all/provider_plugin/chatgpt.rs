use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use indexmap::IndexMap;
use opencode_ai::ExecutionContext;
use opencode_ai::MapEnv;
use opencode_ai::route::compile::{StreamOptions, compile};
use opencode_ai::route::executor::PreparedHttp;
use opencode_ai::schema::{
    AssistantMessage, ContentPart, EffortPart, LlmRequest, Message, SystemMessage, TextPart, UserMessage,
};
use opencode_catalog::builtin::Builtin;
use opencode_catalog::credential::CreateInput;
use opencode_catalog::integration::{Attempt, AttemptMode, OAuthConnectInput};
use opencode_catalog::provider_plugin::hooks::{Compaction, Context, ModelRequest, RequestKind, Retry, RetryDecision};
use opencode_catalog::provider_plugin::storage::Storage;
use opencode_catalog::provider_plugin::{PluginContext, SessionRef, chatgpt, openai};
use opencode_catalog::resolver::{self, Resolver};
use opencode_contract::codec::{JsonMap, decode_json};
use opencode_contract::generated::{
    AgentID, CredentialKey, CredentialKeyType, CredentialOAuth, CredentialOAuthType, CredentialValue, IntegrationID,
    IntegrationMethod, IntegrationMethodID, IntegrationOAuthMethodType, ModelCost, ModelCostCache, ModelInfo,
    ModelInfoLimit, ModelRef, ModelSettings, ModelVariant, ModelVariantSettings, MoneyUSDPerMillionTokens,
    ProviderCompaction, ProviderCompactionSummary, ProviderCompactionSummaryType, ProviderID, ProviderInfo,
    ProviderTransport, SessionID,
};
use serde_json::{Value, json};
use url::Url;

use crate::builtin::{Harness, Input, start};
use crate::provider_plugin::helpers::MemoryKv;
use crate::provider_plugin::support::{
    FakeSessions, Reply, Routes, base_url, headers, plugin_context_with, provider_settings_from, settings_json,
    wait_for_setting,
};

const INTEGRATION_ID: &str = "openai";

const METHOD_ID: &str = "chatgpt-token-sharing";

const LEGACY_METHOD_ID: &str = "chatgpt-browser";

const HEADLESS_METHOD_ID: &str = "chatgpt-headless";

const RESOURCE: &str = "https://api.openai.com/v1";

const CODEX_URL: &str = "https://chatgpt.com/backend-api/codex";

const PACKAGE: &str = "@opencode/ai/providers/openai";

const RESPONSES_PACKAGE: &str = "@opencode/ai/providers/openai/responses";

const FULL: ModelInfoLimit = ModelInfoLimit { context: 1_050_000.0, input: Some(922_000.0), output: 128_000.0 };

const HOST_ID: &str = "urn:uuid:00000000-0000-4000-8000-000000000000";

const JWKS_URL: &str = "http://test/jwks";

const JWKS_N: &str = "uMSlz7bCHaSzzyWLknbqHgtg_dDh9bGKQ8fTw0LK4H3jxJ_Z73r9RnIPgtbKvRe3otQjboSrBkwDJ0uPrfdxhizDVCfO2K5bo6XzFghITK9u7x7J413lGqGwN21qnLL1OnmJzuedBfBTryg5gHagl2HqNVwf2g8pc1kuXzMaA8iyBkNeZSqLn4CucguwqRn5ZMzekAIdQVjttuKHPos40ZoFiHNFvS99q8YdzcQ-ans8gfVGUvkra7Y00XHgmfgrNAPt-CjTJoUW4KT4Uy-lLP1ytubk4NZmbDmpZWNkE-5nhuF7TdGrv3dlyofmq92CnX3wWzJ5RTa_rRwUCUi-Gw";

const JWKS_E: &str = "AQAB";

const PRIVATE_KEY_DER: &str = "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC4xKXPtsIdpLPPJYuSduoeC2D90OH1sYpDx9PDQsrgfePEn9nvev1Gcg+C1sq9F7ei1CNuhKsGTAMnS4+t93GGLMNUJ87YrlujpfMWCEhMr27vHsnjXeUaobA3bWqcsvU6eYnO550F8FOvKDmAdqCXYeo1XB/aDylzWS5fMxoDyLIGQ15lKoufgK5yC7CpGflkzN6QAh1BWO224oc+izjRmgWIc0W9L32rxh3NxD5qezyB9UZS+StrtjTRceCZ+Cs0A+34KNMmhRbgpPhTL6Us/XK25uTg1mZsOallY2QT7meG4XtN0au/d2XKh+ar3YKdffBbMnlFNr+tHBQJSL4bAgMBAAECggEAORc6DLZxwGsVzBnTyIvJx0LI02RpjVswV8U0346R1U/Qwv6Jfn/IJq4zsL/hoQ/lBAD8ebewiJ+QEeI9IQnmIAlQyX4LYdRuKGCsfdBNVrSWWHhw9TocAmCXbGj4mjaJo2nOdQoH8L/Te7m4GsOM13m/voEarmwk6KlTtMK7dRNVO3QMi4xm0BthVzB8tJasgbD0Nf7mS63+9Q9Hik2sMdfR8cby7N4CD198wqeB9vOg+jbpLjGL/VIsm9J/dPXrPlBF0AhLIaZMmrV1n2D+LnF3Ir5Mo8bulLnhyH5Gz7Sfa79AvWPnEgV32qTUgFmKEyac6Mf/wDN4U5j9EmGTkQKBgQDvNpGfqKNTDAHNB6nETqqxt61d4Xx6m9rSPkPHWO1iaXD8XwE1n7y3yLVy9faBGVRg4xEUnRZEvqaNJHaR7/N03ezNpa+Xb+uo6Ize9RLuPmqkORiNyrGWif0up40XRVpvOGmFa0VJbuvXA6j7lTxgO2xdIC9Vx09ETLSmSxML1wKBgQDFu/tfOADIG1GNqvw1Dmnn69v+6Zi4len8YkQf1FuJZejrGrgQuWNQElOXoBmIcUR92C+spwKa1qazwvB+r2lqZ9iFh6tutjO1goxZut8G/PBwOu/mWCplMJXaDqtNAmrRl5TNpmo+sMTgxXI7wStORG1a59ELXk8jtCC57nf3XQKBgH2EkJzKDAkDf95pavcKoB5oVpuwOXTMWuF+smnji4vb7+E8qJHOo2N7+Vm9Hf9XnyLHgqriUlD4CMR+oYip84EbAapWifBYVbtnaqOCzNAeCC7rlqqv9Mcc2LCmUi4nwfAiFZxgURbbEPzdesmn4OpNw+qJfMzxq/IWnaO0OOY5AoGAMK20lp5s4taG3z+G03D29ZeFE15isJ6HehCfAM8AsfSawLZVNhjNsDqyxZMudf6cg/ai1fhPldreM0b8edOpOf7SuRzLQXgthrPa5YQxv8CpWJfJ04VE+qBZY9NUrKK5tVuB0Jb1dPWDomo5UyidBqml2klTl17iUMHZhKyGcCkCgYEAlwRLwHJXyp6/dB+x/Tvu2qJ8SaBA7OIT9yNf8dKHvSmq1t5a7mgZ19AR1sjDcJmZJXs37Wzd02NFT1AveedswAYLC5wirH3EdoOizdzfTHPGbFaMD79Xxq9rwIhaTdSg+276X050lN5Q9M7xo+u/KodyMeiONVVsoCrK2N0GNak=";

fn integration_id() -> IntegrationID {
    IntegrationID(INTEGRATION_ID.to_owned())
}

fn provider_id() -> ProviderID {
    ProviderID(INTEGRATION_ID.to_owned())
}

fn method_id(method: &str) -> IntegrationMethodID {
    IntegrationMethodID(method.to_owned())
}

fn oauth(method: &str, access: &str, refresh: &str, metadata: Option<JsonMap<Value>>) -> CredentialValue {
    CredentialValue::Oauth(CredentialOAuth {
        r#type: CredentialOAuthType::Oauth,
        method_id: method_id(method),
        refresh: refresh.to_owned(),
        access: access.to_owned(),
        expires: 9_999_999_999_999.0,
        metadata,
    })
}

fn sharing(access: &str, client_id: &str) -> CredentialValue {
    oauth(
        METHOD_ID,
        access,
        "refresh",
        Some(JsonMap(vec![("clientID".to_owned(), json!(client_id)), ("scopes".to_owned(), json!([]))])),
    )
}

fn key(value: &str) -> CredentialValue {
    CredentialValue::Key(CredentialKey { r#type: CredentialKeyType::Key, key: value.to_owned(), metadata: None })
}

fn summary() -> ProviderCompaction {
    ProviderCompaction::Summary(ProviderCompactionSummary { r#type: ProviderCompactionSummaryType::Summary })
}

fn cost(input: f64, output: f64) -> ModelCost {
    ModelCost {
        tier: None,
        input: MoneyUSDPerMillionTokens(input),
        output: MoneyUSDPerMillionTokens(output),
        cache: ModelCostCache { read: MoneyUSDPerMillionTokens(0.0), write: MoneyUSDPerMillionTokens(0.0) },
    }
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

async fn add_chatgpt(harness: &mut Harness, context: &PluginContext) {
    let mut builtin = Builtin::default();
    chatgpt::register(&harness.services, context, &mut builtin).await;
    harness.record(builtin);
}

async fn add_openai(harness: &mut Harness, context: &PluginContext) {
    let mut builtin = Builtin::default();
    openai::register(&harness.services, context, &mut builtin).await;
    harness.record(builtin);
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

async fn provider(harness: &Harness) -> ProviderInfo {
    harness
        .services
        .provider
        .get(&provider_id())
        .await
        .expect("the OpenAI provider lookup")
        .expect("the OpenAI provider")
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

fn form(request: &PreparedHttp) -> BTreeMap<String, String> {
    url::form_urlencoded::parse(&request.body).map(|(key, value)| (key.into_owned(), value.into_owned())).collect()
}

fn is_agent_host_id(value: &str) -> bool {
    let Some(uuid) = value.strip_prefix("urn:uuid:") else {
        return false;
    };
    let bytes = uuid.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (index, byte) in bytes.iter().enumerate() {
        match index {
            8 | 13 | 18 | 23 => {
                if *byte != b'-' {
                    return false;
                }
            }
            _ => {
                if !byte.is_ascii_hexdigit() {
                    return false;
                }
            }
        }
    }
    bytes[14] == b'4' && matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
}

fn base64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn sign_token(claims: &Value, alg: &str) -> String {
    let header = json!({ "alg": alg, "kid": "sign-in" });
    let message = format!("{}.{}", base64url(header.to_string().as_bytes()), base64url(claims.to_string().as_bytes()));
    let der = STANDARD.decode(PRIVATE_KEY_DER).expect("the fixture private key decodes");
    let key = ring::signature::RsaKeyPair::from_pkcs8(&der).expect("the fixture private key parses");
    let mut signature = vec![0u8; key.public().modulus_len()];
    key.sign(&ring::signature::RSA_PKCS1_SHA256, &ring::rand::SystemRandom::new(), message.as_bytes(), &mut signature)
        .expect("the fixture key signs the token");
    format!("{message}.{}", base64url(&signature))
}

fn tamper_signature(token: &str) -> String {
    let mut parts = token.split('.');
    let header = parts.next().expect("the token header");
    let payload = parts.next().expect("the token payload");
    let mut signature = URL_SAFE_NO_PAD.decode(parts.next().expect("the token signature")).expect("the signature");
    signature[0] ^= 0x01;
    format!("{header}.{payload}.{}", base64url(&signature))
}

fn claims(nonce: &str, audience: &str, subject: Option<&str>, expires: Option<f64>) -> Value {
    let mut claims = json!({
        "nonce": nonce,
        "iss": "https://auth.openai.com",
        "aud": audience,
    });
    if let Some(subject) = subject {
        claims["sub"] = json!(subject);
    }
    if let Some(expires) = expires {
        claims["exp"] = json!(expires);
    }
    claims
}

fn retry_event(body: &str) -> Retry {
    Retry {
        session_id: SessionID("ses_usage_limit".to_owned()),
        agent: AgentID("build".to_owned()),
        model: ModelRef { provider_id: provider_id(), id: "gpt-5.5".to_owned(), variant: None },
        error: json!({
            "type": "provider.rate-limit",
            "message": "Rate limit exceeded",
            "status": 429,
            "response": { "body": body },
        }),
        attempt: 2,
        decision: RetryDecision::Retry { delay_ms: 1000 },
    }
}

fn model_request(provider: &str, session: &str) -> ModelRequest {
    ModelRequest {
        session_id: SessionID(session.to_owned()),
        agent: AgentID("build".to_owned()),
        model: ModelRef { provider_id: ProviderID(provider.to_owned()), id: "gpt-5.5".to_owned(), variant: None },
        kind: RequestKind::Primary,
        base_url: Some(RESOURCE.to_owned()),
        headers: IndexMap::new(),
    }
}

fn options() -> IndexMap<String, Value> {
    IndexMap::from([("maxTokens".to_owned(), json!(128_000))])
}

fn context_event(provider: &str) -> Context {
    Context {
        session_id: SessionID("ses_test".to_owned()),
        agent: AgentID("build".to_owned()),
        model: ModelRef { provider_id: ProviderID(provider.to_owned()), id: "gpt-5.5".to_owned(), variant: None },
        kind: RequestKind::Primary,
        options: options(),
    }
}

fn compaction_event(provider: &str) -> Compaction {
    Compaction {
        session_id: SessionID("ses_test".to_owned()),
        agent: AgentID("build".to_owned()),
        model: ModelRef { provider_id: ProviderID(provider.to_owned()), id: "gpt-5.5".to_owned(), variant: None },
        kind: RequestKind::Primary,
        options: options(),
        result: None,
    }
}

fn text_part(text: &str) -> ContentPart {
    ContentPart::Text(TextPart { text: text.to_owned(), cache: None, provider_metadata: None })
}

fn user(text: &str) -> Message {
    Message::User(UserMessage { id: None, content: vec![text_part(text)], provider_metadata: None })
}

fn assistant(text: &str) -> Message {
    Message::Assistant(AssistantMessage { id: None, content: vec![text_part(text)], provider_metadata: None })
}

fn effort(effort: &str, previous: &str) -> Message {
    Message::System(SystemMessage {
        id: None,
        content: vec![ContentPart::Effort(EffortPart {
            effort: Some(effort.to_owned()),
            previous: Some(previous.to_owned()),
        })],
        tools: None,
    })
}

fn switched_messages() -> Vec<Message> {
    vec![user("a"), assistant("b"), effort("max", "high"), user("c")]
}

fn switched_back_messages() -> Vec<Message> {
    let mut messages = switched_messages();
    messages.push(assistant("d"));
    messages.push(effort("high", "max"));
    messages.push(user("e"));
    messages
}

fn input_items(body: &Value) -> &[Value] {
    body.get("input").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

fn configuration_efforts(body: &Value) -> Vec<String> {
    input_items(body)
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("configuration_update"))
        .filter_map(|item| item["reasoning"]["effort"].as_str().map(str::to_owned))
        .collect()
}

fn input_texts(body: &Value, role: &str) -> Vec<String> {
    input_items(body)
        .iter()
        .filter(|item| item.get("role").and_then(Value::as_str) == Some(role))
        .filter_map(|item| {
            item.get("content")
                .and_then(Value::as_array)
                .and_then(|content| content.first())
                .and_then(|part| part.get("text"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect()
}

async fn resolve_compaction(
    harness: &Harness,
    resolver: &Resolver,
    id: &str,
    variant: Option<&str>,
) -> Result<Option<ProviderCompaction>, resolver::Error> {
    let model = model(harness, id).await;
    Ok(resolver.resolve_model(&model, variant).await?.compaction)
}

async fn reasoning_body(harness: &Harness, resolver: &Resolver, variant: &str, messages: Vec<Message>) -> Value {
    let model = model(harness, "gpt-6.1-sol").await;
    let resolved = resolver.resolve_model(&model, Some(variant)).await.expect("resolve the reasoning variant");
    compile(
        &resolved.model,
        LlmRequest { messages, ..LlmRequest::default() },
        &StreamOptions::default(),
        &ExecutionContext::system(),
    )
    .expect("compile the reasoning request")
    .body
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revokes_the_refresh_token_with_the_issued_client_id() {
    let routes = Routes::new();
    let calls = Arc::new(AtomicUsize::new(0));
    routes.route_with("POST", "http://test/oauth/revoke", {
        let calls = calls.clone();
        move |_| {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return Reply::status(200);
            }
            Reply::status(503)
        }
    });
    let harness = start(Input::new(Vec::new())).await;
    let context = plugin_context_with(&harness, routes.clone());
    let CredentialValue::Oauth(value) = oauth(
        METHOD_ID,
        "access",
        "refresh-token",
        Some(JsonMap(vec![("clientID".to_owned(), json!("oaiapp_issued"))])),
    ) else {
        panic!("the ChatGPT credential is OAuth")
    };

    chatgpt::revoke(context.http.as_ref(), &context.app.useragent(), "http://test/oauth/revoke", &value)
        .await
        .expect("the first revocation succeeds");
    assert!(
        chatgpt::revoke(context.http.as_ref(), &context.app.useragent(), "http://test/oauth/revoke", &value)
            .await
            .is_err()
    );

    let expected: BTreeMap<String, String> =
        [("token", "refresh-token"), ("token_type_hint", "refresh_token"), ("client_id", "oaiapp_issued")]
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect();
    let forms: Vec<BTreeMap<String, String>> = routes.requests().iter().map(form).collect();
    assert_eq!(forms, vec![expected.clone(), expected]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verifies_a_signed_id_token_against_openai_issuer_client_id_expiry_and_nonce() {
    let routes = Routes::new();
    routes.route_json(
        "GET",
        JWKS_URL,
        json!({
            "keys": [{ "kty": "RSA", "kid": "sign-in", "alg": "RS256", "use": "sig", "n": JWKS_N, "e": JWKS_E }],
        }),
    );
    let harness = start(Input::new(Vec::new())).await;
    let context = plugin_context_with(&harness, routes);
    let now = (opencode_util::time::now_ms() / 1000) as f64;
    let expires = now + 60.0;

    let valid = sign_token(&claims("expected", "oaiapp_issued", Some("user_123"), Some(expires)), "RS256");
    chatgpt::verify_id_token(context.http.as_ref(), &valid, "oaiapp_issued", "expected", JWKS_URL)
        .await
        .expect("the valid ID token verifies");

    let mut wrong_issuer = claims("expected", "oaiapp_issued", Some("user_123"), Some(expires));
    wrong_issuer["iss"] = json!("https://someone-else.example");
    let missing_subject = sign_token(&claims("expected", "oaiapp_issued", None, Some(expires)), "RS256");
    let blank_subject = sign_token(&claims("expected", "oaiapp_issued", Some(" "), Some(expires)), "RS256");
    let no_expiry = sign_token(&claims("expected", "oaiapp_issued", Some("user_123"), None), "RS256");
    let expired = sign_token(&claims("expected", "oaiapp_issued", Some("user_123"), Some(expires - 120.0)), "RS256");
    let wrong_algorithm = sign_token(&claims("expected", "oaiapp_issued", Some("user_123"), Some(expires)), "HS256");
    let wrong_issuer_token = sign_token(&wrong_issuer, "RS256");
    let tampered = tamper_signature(&valid);

    let cases = [
        ("missing subject", missing_subject.clone(), "oaiapp_issued", "expected"),
        ("blank subject", blank_subject.clone(), "oaiapp_issued", "expected"),
        ("wrong nonce", valid.clone(), "oaiapp_issued", "wrong"),
        ("wrong audience", valid.clone(), "wrong-client", "expected"),
        ("wrong issuer", wrong_issuer_token.clone(), "oaiapp_issued", "expected"),
        ("missing expiry", no_expiry.clone(), "oaiapp_issued", "expected"),
        ("expired", expired.clone(), "oaiapp_issued", "expected"),
        ("tampered signature", tampered.clone(), "oaiapp_issued", "expected"),
        ("wrong algorithm", wrong_algorithm.clone(), "oaiapp_issued", "expected"),
    ];
    for (case, token, client_id, nonce) in cases {
        assert!(
            chatgpt::verify_id_token(context.http.as_ref(), &token, client_id, nonce, JWKS_URL).await.is_err(),
            "{case}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registers_the_chatgpt_oauth_method() {
    let mut harness = start(Input::new(Vec::new())).await;
    let context = plugin_context_with(&harness, Routes::new());
    add_chatgpt(&mut harness, &context).await;
    let integration = harness
        .services
        .integration
        .get(&integration_id())
        .await
        .expect("get the OpenAI integration")
        .expect("the OpenAI integration");
    assert_eq!(integration.methods.len(), 1);
    let IntegrationMethod::Oauth(method) = &integration.methods[0] else { panic!("the ChatGPT OAuth method is first") };
    assert_eq!(method.id, method_id(METHOD_ID));
    assert_eq!(method.r#type, IntegrationOAuthMethodType::Oauth);
    assert_eq!(method.label, "Sign in with ChatGPT");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keeps_the_codex_browser_and_headless_methods_alongside_token_sharing() {
    let mut harness = start(Input::new(Vec::new())).await;
    let context = plugin_context_with(&harness, Routes::new());
    add_chatgpt(&mut harness, &context).await;
    add_openai(&mut harness, &context).await;
    let integration = harness
        .services
        .integration
        .get(&integration_id())
        .await
        .expect("get the OpenAI integration")
        .expect("the OpenAI integration");
    let ids: Vec<&str> = integration
        .methods
        .iter()
        .filter_map(|method| match method {
            IntegrationMethod::Oauth(method) => Some(method.id.0.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(ids, [METHOD_ID, LEGACY_METHOD_ID, HEADLESS_METHOD_ID]);
}

async fn stops_sharing_retries(source: &str, code: &str) {
    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential(INTEGRATION_ID, sharing("chatgpt-token", "oaiapp_issued")).await;
    let context = plugin_context_with(&harness, Routes::new());
    add_chatgpt(&mut harness, &context).await;
    let body = match source {
        "http" => json!({ "error": { "code": code } }),
        _ => json!({ "type": "response.failed", "response": { "error": { "code": code } } }),
    }
    .to_string();

    let event = context.hooks.trigger_retry(retry_event(&body)).await;
    assert_eq!(event.decision, RetryDecision::Stop);
    let other = context.hooks.trigger_retry(retry_event("{}")).await;
    assert_eq!(other.decision, RetryDecision::Retry { delay_ms: 1000 });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stops_retries_for_subscription_sharing_usage_limit_exceeded_from_http_failures() {
    stops_sharing_retries("http", "subscription_sharing_usage_limit_exceeded").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stops_retries_for_subscription_sharing_usage_limit_exceeded_from_stream_failures() {
    stops_sharing_retries("stream", "subscription_sharing_usage_limit_exceeded").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stops_retries_for_subscription_sharing_workplace_individual_usage_limit_exceeded_from_http_failures() {
    stops_sharing_retries("http", "subscription_sharing_workplace_individual_usage_limit_exceeded").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn leaves_api_key_openai_retries_alone() {
    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential(INTEGRATION_ID, key("sk-test")).await;
    let context = plugin_context_with(&harness, Routes::new());
    add_chatgpt(&mut harness, &context).await;
    let body = json!({ "error": { "code": "subscription_sharing_usage_limit_exceeded" } }).to_string();
    let event = context.hooks.trigger_retry(retry_event(&body)).await;
    assert_eq!(event.decision, RetryDecision::Retry { delay_ms: 1000 });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stops_deterministic_chatgpt_stream_failures_while_retrying_temporary_unavailability() {
    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential(INTEGRATION_ID, sharing("chatgpt-token", "oaiapp_issued")).await;
    let context = plugin_context_with(&harness, Routes::new());
    add_chatgpt(&mut harness, &context).await;

    for code in [
        "subscription_sharing_user_not_eligible",
        "subscription_sharing_unsupported_capability",
        "subscription_sharing_route_not_supported",
        "subscription_sharing_invalid_user",
        "chatpass_v2_scope_not_authorized",
        "chatpass_v2_invalid_authorization_context",
    ] {
        let body = json!({ "type": "response.failed", "response": { "error": { "code": code } } }).to_string();
        let event = context.hooks.trigger_retry(retry_event(&body)).await;
        assert_eq!(event.decision, RetryDecision::Stop, "{code}");
    }
    for code in ["subscription_sharing_usage_unavailable", "subscription_sharing_user_unavailable"] {
        let body = json!({ "type": "response.failed", "response": { "error": { "code": code } } }).to_string();
        let event = context.hooks.trigger_retry(retry_event(&body)).await;
        assert_eq!(event.decision, RetryDecision::Retry { delay_ms: 1000 }, "{code}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registers_a_new_chatgpt_agent_on_first_sign_in_with_a_loopback_callback() {
    let kv = Arc::new(MemoryKv::default());
    let mut harness = start(Input::new(Vec::new())).await;
    let mut context = plugin_context_with(&harness, Routes::new());
    context.kv = kv.clone();
    add_chatgpt(&mut harness, &context).await;

    let attempt = connect(&harness, METHOD_ID).await;
    let url = Url::parse(&attempt.url).expect("the ChatGPT authorization url");
    assert_eq!(attempt.mode, AttemptMode::Auto);
    assert_eq!(url.origin().ascii_serialization(), "https://auth.openai.com");
    assert_eq!(url.path(), "/api/accounts/authorize");
    assert_eq!(param(&url, "client_id").as_deref(), Some("dynamic_agent_client"));
    assert_eq!(param(&url, "agent_name_hint").as_deref(), Some("OpenCode"));
    assert_eq!(param(&url, "response_type").as_deref(), Some("code"));
    assert_eq!(
        param(&url, "scope").as_deref(),
        Some("openid profile email offline_access resource.invoke chatgpt.tokens.use.direct")
    );
    assert_eq!(param(&url, "resource").as_deref(), Some(RESOURCE));
    assert_eq!(param(&url, "code_challenge_method").as_deref(), Some("S256"));
    assert!(param(&url, "state").is_some_and(|state| !state.is_empty()));
    assert!(param(&url, "nonce").is_some_and(|nonce| !nonce.is_empty()));
    assert!(param(&url, "code_challenge").is_some_and(|challenge| !challenge.is_empty()));

    let host_id = param(&url, "ext_agent_host_id").expect("the agent host id");
    assert!(is_agent_host_id(&host_id), "{host_id}");
    let stored = Storage::new(kv.clone(), chatgpt::ID).get("chatgpt-agent-host-id").await;
    assert_eq!(stored, Some(json!(host_id)));

    let second = connect(&harness, METHOD_ID).await;
    let second_url = Url::parse(&second.url).expect("the second ChatGPT authorization url");
    assert_eq!(param(&second_url, "ext_agent_host_id").as_deref(), Some(host_id.as_str()));

    let redirect = Url::parse(&param(&url, "redirect_uri").expect("the ChatGPT redirect")).expect("a redirect url");
    assert_eq!(redirect.host_str(), Some("127.0.0.1"));
    assert_eq!(redirect.path(), "/auth/callback");
    assert!(redirect.port().is_some_and(|port| port > 0));

    harness.services.integration.oauth().cancel(&integration_id(), &attempt.attempt_id).await;
    harness.services.integration.oauth().cancel(&integration_id(), &second.attempt_id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn waits_to_show_browser_success_until_the_sign_in_callback_succeeds() {
    let mut harness = start(Input::new(Vec::new())).await;
    let context = plugin_context_with(&harness, Routes::new());
    add_chatgpt(&mut harness, &context).await;
    let attempt = connect(&harness, METHOD_ID).await;
    let authorization = Url::parse(&attempt.url).expect("the ChatGPT authorization url");
    let mut callback =
        Url::parse(&param(&authorization, "redirect_uri").expect("the ChatGPT redirect")).expect("a redirect url");
    set_param(&mut callback, "code", "test-code");
    set_param(&mut callback, "state", &param(&authorization, "state").expect("the ChatGPT state"));

    let response =
        opencode_config::http::client().get(callback.as_str()).send().await.expect("fetch the ChatGPT callback");
    assert_eq!(response.status(), 400);
    assert!(response.text().await.expect("the ChatGPT callback page").contains("did not return a client ID"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reauthorizes_with_the_client_id_issued_to_an_existing_chatgpt_connection() {
    let kv = Arc::new(MemoryKv::default());
    Storage::new(kv.clone(), chatgpt::ID).set("chatgpt-agent-host-id", json!(HOST_ID)).await;
    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential(INTEGRATION_ID, sharing("chatgpt-token", "oaiapp_issued")).await;
    let mut context = plugin_context_with(&harness, Routes::new());
    context.kv = kv.clone();
    add_chatgpt(&mut harness, &context).await;

    let attempt = connect(&harness, METHOD_ID).await;
    let url = Url::parse(&attempt.url).expect("the ChatGPT authorization url");
    assert_eq!(param(&url, "client_id").as_deref(), Some("oaiapp_issued"));
    assert!(param(&url, "agent_name_hint").is_none());
    assert_eq!(param(&url, "ext_agent_host_id").as_deref(), Some(HOST_ID));
    assert_eq!(
        Url::parse(&param(&url, "redirect_uri").expect("the ChatGPT redirect")).expect("a redirect url").host_str(),
        Some("127.0.0.1")
    );
    harness.services.integration.oauth().cancel(&integration_id(), &attempt.attempt_id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_a_reauthorization_callback_that_returns_a_different_client_id() {
    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential(INTEGRATION_ID, sharing("chatgpt-token", "oaiapp_issued")).await;
    let context = plugin_context_with(&harness, Routes::new());
    add_chatgpt(&mut harness, &context).await;
    let attempt = connect(&harness, METHOD_ID).await;
    let authorization = Url::parse(&attempt.url).expect("the ChatGPT authorization url");
    assert_eq!(param(&authorization, "client_id").as_deref(), Some("oaiapp_issued"));
    let mut callback =
        Url::parse(&param(&authorization, "redirect_uri").expect("the ChatGPT redirect")).expect("a redirect url");
    set_param(&mut callback, "code", "test-code");
    set_param(&mut callback, "state", &param(&authorization, "state").expect("the ChatGPT state"));
    set_param(&mut callback, "client_id", "oaiapp_other");

    let response =
        opencode_config::http::client().get(callback.as_str()).send().await.expect("fetch the ChatGPT callback");
    assert_eq!(response.status(), 400);
    assert!(response.text().await.expect("the ChatGPT callback page").contains("different client"));
    assert_eq!(harness.services.credentials.list(&integration_id()).expect("list the OpenAI credentials").len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn moves_only_token_sharing_credentials_to_the_new_method_id_and_caches_their_saved_models() {
    let kv = Arc::new(MemoryKv::default());
    let mut harness = start(Input::new(Vec::new())).await;
    let legacy = harness
        .services
        .credentials
        .create(CreateInput {
            id: None,
            integration_id: integration_id(),
            value: oauth(
                LEGACY_METHOD_ID,
                "codex-token",
                "codex-refresh",
                Some(JsonMap(vec![("accountID".to_owned(), json!("acct_123"))])),
            ),
            label: None,
            activate: None,
        })
        .expect("create the legacy Codex credential");
    let snapshot = json!([{
        "slug": "gpt-5.5",
        "display_name": "GPT-5.5",
        "visibility": "list",
        "supported_in_api": true,
        "context_window": 272_000,
        "input_modalities": ["text"],
    }]);
    let sharing = harness
        .services
        .credentials
        .create(CreateInput {
            id: None,
            integration_id: integration_id(),
            value: oauth(
                LEGACY_METHOD_ID,
                "sharing-token",
                "sharing-refresh",
                Some(JsonMap(vec![
                    ("clientID".to_owned(), json!("oaiapp_issued")),
                    ("models".to_owned(), snapshot.clone()),
                ])),
            ),
            label: None,
            activate: None,
        })
        .expect("create the legacy sharing credential");
    let mut context = plugin_context_with(&harness, Routes::new());
    context.kv = kv.clone();
    add_chatgpt(&mut harness, &context).await;

    let legacy = harness.services.credentials.get(&legacy.id).expect("get the legacy credential").expect("legacy");
    let CredentialValue::Oauth(legacy) = &legacy.value else { panic!("the legacy credential is OAuth") };
    assert_eq!(legacy.method_id, method_id(LEGACY_METHOD_ID));

    let moved = harness.services.credentials.get(&sharing.id).expect("get the sharing credential").expect("sharing");
    let CredentialValue::Oauth(moved) = &moved.value else { panic!("the sharing credential is OAuth") };
    assert_eq!(moved.method_id, method_id(METHOD_ID));
    assert_eq!(moved.access, "sharing-token");
    let metadata = moved.metadata.as_ref().expect("the sharing credential metadata");
    assert_eq!(
        metadata.0.iter().find(|(key, _)| key == "clientID").map(|(_, value)| value),
        Some(&json!("oaiapp_issued"))
    );
    assert!(metadata.0.iter().all(|(key, _)| key != "models"));

    assert_eq!(Storage::new(kv.clone(), chatgpt::ID).get("models:oaiapp_issued").await, Some(snapshot));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn routes_chatgpt_over_http_with_only_the_listed_catalog_fallback_models() {
    let mut harness = start(Input::new(Vec::new())).await;
    let seeded = harness
        .services
        .provider
        .transform(|editor| {
            let provider = provider_id();
            editor.update(&provider, |provider| {
                provider.package = PACKAGE.to_owned();
                provider.settings = Some(provider_settings_from(json!({ "transport": "websocket" })));
            });
            editor.models_update(&provider, "gpt-5.5", |model| {
                model.limit = FULL;
                model.cost = vec![cost(1.0, 2.0)];
            });
            editor.models_update(&provider, "gpt-5.5-pro", |_| {});
            editor.models_update(&provider, "gpt-5.4", |model| {
                model.limit = ModelInfoLimit { context: 1_050_000.0, input: Some(922_000.0), output: 64_000.0 };
            });
            editor.models_update(&provider, "gpt-5.4-pro", |model| {
                model.model_id = "gpt-5.4".to_owned();
                model.body = Some(JsonMap(vec![("reasoning".to_owned(), json!({ "mode": "pro" }))]));
            });
            editor.models_update(&provider, "gpt-5.6", |_| {});
            editor.models_update(&provider, "gpt-5.6-sol", |model| model.limit = FULL);
            editor.models_update(&provider, "gpt-4.1", |_| {});
            for id in ["gpt-6-astra", "gpt-5.10"] {
                editor.models_update(&provider, id, |model| model.limit = FULL);
            }
            editor.models_update(&provider, "gpt-5", |_| {});
            editor.models_update(&provider, "gpt-5.04-astra", |_| {});
            editor.models_update(&provider, "gpt-4.99", |_| {});
            for id in [
                "gpt-5.5-fast",
                "gpt-5.6-luna",
                "gpt-5.6-luna-fast",
                "gpt-5.6-sol-fast",
                "gpt-5.6-terra",
                "gpt-5.6-terra-fast",
                "gpt-6-astra-fast",
                "gpt-6-luna",
                "gpt-6-luna-fast",
                "gpt-6-sol",
                "gpt-6-sol-fast",
                "gpt-6.1-sol",
                "gpt-6.1-sol-fast",
                "gpt-6.1-terra",
                "gpt-6.1-terra-fast",
            ] {
                editor.models_update(&provider, id, |_| {});
            }
            Ok(())
        })
        .await;
    harness.registrations.registrations.push(seeded);
    harness.credential(INTEGRATION_ID, oauth(METHOD_ID, "chatgpt-token", "refresh", None)).await;

    let mut fake = FakeSessions::default();
    fake.sessions.insert(
        "ses_child".to_owned(),
        SessionRef {
            id: SessionID("ses_child".to_owned()),
            parent_id: Some(SessionID("ses_parent".to_owned())),
            fork_session_id: None,
        },
    );
    let mut context = plugin_context_with(&harness, Routes::new());
    context.sessions = Arc::new(fake);
    add_chatgpt(&mut harness, &context).await;

    let direct = context.hooks.trigger_model_request(model_request(INTEGRATION_ID, "ses_test")).await;
    let child = context.hooks.trigger_model_request(model_request(INTEGRATION_ID, "ses_child")).await;

    let info = provider(&harness).await;
    assert_eq!(info.package, PACKAGE);
    let settings = settings_json(&info).expect("the OpenAI provider settings");
    assert_eq!(settings.get("transport"), Some(&json!("http")));
    assert_eq!(settings.get("baseURL"), Some(&json!(RESOURCE)));
    assert_eq!(settings.get("compaction"), Some(&json!({ "type": "summary" })));
    assert_eq!(settings.get("encoding"), Some(&json!("zstd")));
    assert!(!headers(&info).unwrap_or_default().contains_key("x-openai-chatpass-test"));

    assert_eq!(direct.base_url.as_deref(), Some(RESOURCE));
    assert_eq!(
        direct.headers,
        IndexMap::from([
            ("session-id".to_owned(), "ses_test".to_owned()),
            ("thread-id".to_owned(), "ses_test".to_owned()),
            ("x-client-request-id".to_owned(), "ses_test".to_owned()),
        ])
    );
    assert_eq!(
        child.headers,
        IndexMap::from([
            ("session-id".to_owned(), "ses_parent".to_owned()),
            ("thread-id".to_owned(), "ses_child".to_owned()),
            ("x-client-request-id".to_owned(), "ses_child".to_owned()),
        ])
    );

    let eligible = model(&harness, "gpt-5.5").await;
    assert_eq!(eligible.package.as_deref(), Some(PACKAGE));
    assert!(
        eligible
            .headers
            .as_ref()
            .is_none_or(|headers| headers.0.iter().all(|(key, _)| key != "x-openai-chatpass-test"))
    );
    assert!(eligible.cost.is_empty());
    assert_eq!(eligible.limit, FULL);
    assert!(eligible.enabled);
    let gpt_5_6_sol = model(&harness, "gpt-5.6-sol").await;
    assert!(gpt_5_6_sol.enabled);
    assert_eq!(gpt_5_6_sol.limit, FULL);
    assert!(model(&harness, "gpt-6-astra").await.enabled);

    for id in [
        "gpt-5.5-pro",
        "gpt-5.4-pro",
        "gpt-5.4",
        "gpt-5.6",
        "gpt-4.1",
        "gpt-5.10",
        "gpt-5",
        "gpt-5.04-astra",
        "gpt-4.99",
    ] {
        assert!(
            harness.services.model.get(&provider_id(), id).await.expect("the OpenAI model lookup").is_none(),
            "{id}"
        );
    }

    let mut ids: Vec<String> = harness
        .services
        .model
        .available(None)
        .await
        .expect("the available OpenAI models")
        .into_iter()
        .filter(|model| model.provider_id == provider_id())
        .map(|model| model.id)
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        [
            "gpt-5.5",
            "gpt-5.5-fast",
            "gpt-5.6-luna",
            "gpt-5.6-luna-fast",
            "gpt-5.6-sol",
            "gpt-5.6-sol-fast",
            "gpt-5.6-terra",
            "gpt-5.6-terra-fast",
            "gpt-6-astra",
            "gpt-6-astra-fast",
            "gpt-6-luna",
            "gpt-6-luna-fast",
            "gpt-6-sol",
            "gpt-6-sol-fast",
            "gpt-6.1-sol",
            "gpt-6.1-sol-fast",
            "gpt-6.1-terra",
            "gpt-6.1-terra-fast",
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keeps_the_token_sharing_safeguards_when_the_token_cannot_be_refreshed_at_load() {
    let mut harness = start(Input::new(Vec::new())).await;
    let seeded = harness
        .services
        .provider
        .transform(|editor| {
            let provider = provider_id();
            editor.update(&provider, |provider| provider.package = PACKAGE.to_owned());
            editor.models_update(&provider, "gpt-6-astra", |_| {});
            Ok(())
        })
        .await;
    harness.registrations.registrations.push(seeded);
    harness
        .services
        .credentials
        .create(CreateInput {
            id: None,
            integration_id: integration_id(),
            value: CredentialValue::Oauth(CredentialOAuth {
                r#type: CredentialOAuthType::Oauth,
                method_id: method_id(METHOD_ID),
                refresh: "refresh".to_owned(),
                access: "stale-token".to_owned(),
                expires: 1.0,
                metadata: None,
            }),
            label: None,
            activate: None,
        })
        .expect("create the stale sharing credential");

    let context = plugin_context_with(&harness, Routes::new());
    add_chatgpt(&mut harness, &context).await;
    add_openai(&mut harness, &context).await;

    let info = provider(&harness).await;
    let settings = info.settings.as_ref().expect("the OpenAI provider settings");
    assert_eq!(settings.transport, Some(ProviderTransport::Http));
    assert_eq!(settings.compaction, Some(summary()));
    let astra = model(&harness, "gpt-6-astra").await;
    assert_eq!(
        astra.compatibility.as_ref().and_then(|compatibility| compatibility.supports_effort_updates),
        Some(false)
    );
    assert_eq!(model_settings(&astra).get("compaction"), Some(&json!({ "type": "summary" })));
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

    let context = plugin_context_with(&harness, Routes::new());
    add_chatgpt(&mut harness, &context).await;
    add_openai(&mut harness, &context).await;
    let direct = context.hooks.trigger_model_request(model_request(INTEGRATION_ID, "ses_test")).await;

    let info = provider(&harness).await;
    let settings = info.settings.as_ref().expect("the OpenAI provider settings");
    assert_eq!(settings.transport, Some(ProviderTransport::Websocket));
    assert_eq!(settings.encoding, None);
    let provider_headers = headers(&info).unwrap_or_default();
    assert!(!provider_headers.contains_key("x-codex-beta-features"));
    assert!(!provider_headers.contains_key("originator"));

    let full = model(&harness, "gpt-5.5").await;
    assert_eq!(full.package.as_deref(), Some(PACKAGE));
    assert!(full.enabled);
    assert_eq!(full.limit, FULL);
    assert!(model_settings(&full).get("transport").is_none());
    assert!(direct.headers.is_empty());
    assert_eq!(direct.base_url.as_deref(), Some(RESOURCE));
    assert!(model(&harness, "gpt-4.1").await.enabled);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn switches_between_token_sharing_and_codex_credentials_without_mixing_their_routes() {
    let mut harness = start(Input::new(Vec::new())).await;
    let seeded = harness
        .services
        .provider
        .transform(|editor| {
            let provider = provider_id();
            editor.update(&provider, |provider| provider.package = PACKAGE.to_owned());
            editor.models_update(&provider, "gpt-5.5", |_| {});
            Ok(())
        })
        .await;
    harness.registrations.registrations.push(seeded);
    let legacy = harness
        .services
        .credentials
        .create(CreateInput {
            id: None,
            integration_id: integration_id(),
            value: oauth(
                LEGACY_METHOD_ID,
                "codex-token",
                "codex-refresh",
                Some(JsonMap(vec![("accountID".to_owned(), json!("acct_123"))])),
            ),
            label: None,
            activate: None,
        })
        .expect("create the legacy Codex credential");
    let shared_credential = harness
        .services
        .credentials
        .create(CreateInput {
            id: None,
            integration_id: integration_id(),
            value: sharing("sharing-token", "oaiapp_issued"),
            label: None,
            activate: None,
        })
        .expect("create the sharing credential");

    let context = plugin_context_with(&harness, Routes::new());
    add_chatgpt(&mut harness, &context).await;
    add_openai(&mut harness, &context).await;
    assert_eq!(base_url(&provider(&harness).await).as_deref(), Some(RESOURCE));

    harness.services.credentials.activate(&legacy.id).expect("activate the legacy Codex credential");
    let codex = wait_for_setting(&harness, INTEGRATION_ID, "baseURL", CODEX_URL).await;
    assert_eq!(base_url(&codex).as_deref(), Some(CODEX_URL));
    wait_for_header(&harness, "chatgpt-account-id", "acct_123").await;

    harness.services.credentials.activate(&shared_credential.id).expect("activate the sharing credential");
    let direct = wait_for_setting(&harness, INTEGRATION_ID, "baseURL", RESOURCE).await;
    assert_eq!(base_url(&direct).as_deref(), Some(RESOURCE));
    assert_eq!(
        direct.settings.as_ref().expect("the OpenAI provider settings").transport,
        Some(ProviderTransport::Http)
    );
    assert!(!headers(&direct).unwrap_or_default().contains_key("chatgpt-account-id"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn omits_the_default_output_limit_from_openai_steps_and_compaction() {
    let mut harness = start(Input::new(Vec::new())).await;
    let context = plugin_context_with(&harness, Routes::new());
    add_chatgpt(&mut harness, &context).await;
    add_openai(&mut harness, &context).await;

    let openai_context = context.hooks.trigger_context(context_event(INTEGRATION_ID)).await;
    assert!(!openai_context.options.contains_key("maxTokens"));
    let openai_compaction = context.hooks.trigger_compaction(compaction_event(INTEGRATION_ID)).await;
    assert!(!openai_compaction.options.contains_key("maxTokens"));

    let azure_context = context.hooks.trigger_context(context_event("azure")).await;
    assert_eq!(azure_context.options.get("maxTokens"), Some(&json!(128_000)));
    let azure_compaction = context.hooks.trigger_compaction(compaction_event("azure")).await;
    assert_eq!(azure_compaction.options.get("maxTokens"), Some(&json!(128_000)));
}

async fn seed_responses_compaction(harness: &mut Harness) {
    let seeded = harness
        .services
        .provider
        .transform(|editor| {
            let provider = provider_id();
            editor.update(&provider, |provider| provider.package = RESPONSES_PACKAGE.to_owned());
            for id in ["gpt-5.5", "gpt-5.6-sol", "gpt-6-astra"] {
                editor.models_update(&provider, id, |model| model.model_id = "gpt-5.5".to_owned());
            }
            editor.models_update(&provider, "gpt-5.6-sol", |model| {
                model.settings = Some(
                    decode_json::<ModelSettings>(r#"{"compaction":{"type":"native"}}"#)
                        .expect("the native compaction settings"),
                );
            });
            Ok(())
        })
        .await;
    harness.registrations.registrations.push(seeded);
}

fn compaction_resolver(harness: &Harness) -> Arc<Resolver> {
    Resolver::new(
        harness.services.provider.clone(),
        harness.services.model.clone(),
        harness.services.integration.clone(),
        Arc::new(MapEnv::default()),
        Arc::new(resolver::NoBridge),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chatgpt_compaction_defaults_to_summary_and_survives_a_later_provider_default() {
    let mut harness = start(Input::new(Vec::new())).await;
    seed_responses_compaction(&mut harness).await;
    harness.credential(INTEGRATION_ID, sharing("chatgpt-token", "oaiapp_issued")).await;
    let context = plugin_context_with(&harness, Routes::new());
    add_chatgpt(&mut harness, &context).await;
    add_openai(&mut harness, &context).await;
    let resolver = compaction_resolver(&harness);

    assert_eq!(
        resolve_compaction(&harness, resolver.as_ref(), "gpt-5.5", None).await.expect("the base compaction"),
        Some(summary())
    );
    assert_eq!(
        resolve_compaction(&harness, resolver.as_ref(), "gpt-5.6-sol", None).await.expect("the model compaction"),
        Some(summary())
    );

    let provider_default = harness
        .services
        .provider
        .transform(|editor| {
            let provider = provider_id();
            editor.update(&provider, |provider| {
                provider.settings = Some(provider_settings_from(json!({ "compaction": { "type": "native" } })));
            });
            Ok(())
        })
        .await;
    harness.registrations.registrations.push(provider_default);
    assert_eq!(
        resolve_compaction(&harness, resolver.as_ref(), "gpt-5.5", None).await.expect("the ChatGPT base compaction"),
        Some(summary())
    );

    let model_override = harness
        .services
        .model
        .transform(|editor| {
            let provider = provider_id();
            editor.update(&provider, "gpt-6-astra", |model| {
                model.settings = Some(
                    decode_json::<ModelSettings>(r#"{"compaction":{"type":"native"}}"#)
                        .expect("the native compaction settings"),
                );
                model.variants = vec![ModelVariant {
                    id: "high".to_owned(),
                    settings: Some(
                        decode_json::<ModelVariantSettings>(r#"{"compaction":{"type":"native"}}"#)
                            .expect("the native variant compaction settings"),
                    ),
                    headers: None,
                    body: None,
                }];
            });
            Ok(())
        })
        .await;
    harness.registrations.registrations.push(model_override);
    let error = resolve_compaction(&harness, resolver.as_ref(), "gpt-6-astra", Some("high"))
        .await
        .expect_err("native compaction is rejected");
    assert!(matches!(error, resolver::Error::UnsupportedCompaction { ref model_id, .. } if model_id == "gpt-6-astra"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn key_compaction_follows_provider_defaults_until_native_is_rejected() {
    let mut harness = start(Input::new(Vec::new())).await;
    seed_responses_compaction(&mut harness).await;
    harness.credential(INTEGRATION_ID, key("sk-test")).await;
    let context = plugin_context_with(&harness, Routes::new());
    add_chatgpt(&mut harness, &context).await;
    add_openai(&mut harness, &context).await;
    let resolver = compaction_resolver(&harness);

    assert_eq!(
        resolve_compaction(&harness, resolver.as_ref(), "gpt-5.5", None).await.expect("the base compaction"),
        None
    );

    let provider_default = harness
        .services
        .provider
        .transform(|editor| {
            let provider = provider_id();
            editor.update(&provider, |provider| {
                provider.settings = Some(provider_settings_from(json!({ "compaction": { "type": "summary" } })));
            });
            Ok(())
        })
        .await;
    harness.registrations.registrations.push(provider_default);
    assert_eq!(
        resolve_compaction(&harness, resolver.as_ref(), "gpt-5.5", None).await.expect("the summary compaction"),
        Some(summary())
    );

    let native_default = harness
        .services
        .provider
        .transform(|editor| {
            let provider = provider_id();
            editor.update(&provider, |provider| {
                provider.settings = Some(provider_settings_from(json!({ "compaction": { "type": "native" } })));
            });
            Ok(())
        })
        .await;
    harness.registrations.registrations.push(native_default);
    let error = resolve_compaction(&harness, resolver.as_ref(), "gpt-5.5", None)
        .await
        .expect_err("native compaction is rejected");
    assert!(matches!(error, resolver::Error::UnsupportedCompaction { ref model_id, .. } if model_id == "gpt-5.5"));
}

async fn seed_reasoning_variants(harness: &mut Harness) {
    let seeded = harness
        .services
        .provider
        .transform(|editor| {
            let provider = provider_id();
            editor.update(&provider, |provider| {
                provider.package = PACKAGE.to_owned();
                provider.settings = Some(provider_settings_from(json!({ "encoding": "none" })));
            });
            editor.models_update(&provider, "gpt-6.1-sol", |model| {
                model.variants = ["high", "max"]
                    .into_iter()
                    .map(|effort| ModelVariant {
                        id: effort.to_owned(),
                        settings: Some(
                            decode_json::<ModelVariantSettings>(&json!({ "reasoningEffort": effort }).to_string())
                                .expect("the reasoning variant settings"),
                        ),
                        headers: None,
                        body: None,
                    })
                    .collect();
            });
            Ok(())
        })
        .await;
    harness.registrations.registrations.push(seeded);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chatgpt_sends_mid_session_reasoning_effort_switches_as_top_level_reasoning() {
    let mut harness = start(Input::new(Vec::new())).await;
    seed_reasoning_variants(&mut harness).await;
    harness.credential(INTEGRATION_ID, sharing("chatgpt-token", "oaiapp_issued")).await;
    let context = plugin_context_with(&harness, Routes::new());
    add_chatgpt(&mut harness, &context).await;
    add_openai(&mut harness, &context).await;
    let resolver = compaction_resolver(&harness);

    let max_body = reasoning_body(&harness, resolver.as_ref(), "max", switched_messages()).await;
    assert_eq!(max_body["reasoning"]["effort"], json!("max"));
    assert!(configuration_efforts(&max_body).is_empty());
    assert_eq!(input_texts(&max_body, "user"), ["a", "c"]);
    assert_eq!(input_texts(&max_body, "assistant"), ["b"]);

    let high_body = reasoning_body(&harness, resolver.as_ref(), "high", switched_back_messages()).await;
    assert_eq!(high_body["reasoning"]["effort"], json!("high"));
    assert!(configuration_efforts(&high_body).is_empty());
    assert_eq!(input_texts(&high_body, "user"), ["a", "c", "e"]);
    assert_eq!(input_texts(&high_body, "assistant"), ["b", "d"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn key_sends_mid_session_reasoning_effort_switches_as_configuration_updates() {
    let mut harness = start(Input::new(Vec::new())).await;
    seed_reasoning_variants(&mut harness).await;
    harness.credential(INTEGRATION_ID, key("sk-test")).await;
    let context = plugin_context_with(&harness, Routes::new());
    add_chatgpt(&mut harness, &context).await;
    add_openai(&mut harness, &context).await;
    let resolver = compaction_resolver(&harness);

    let max_body = reasoning_body(&harness, resolver.as_ref(), "max", switched_messages()).await;
    assert_eq!(max_body["reasoning"]["effort"], json!("high"));
    assert_eq!(configuration_efforts(&max_body), ["max"]);

    let high_body = reasoning_body(&harness, resolver.as_ref(), "high", switched_back_messages()).await;
    assert_eq!(high_body["reasoning"]["effort"], json!("high"));
    assert_eq!(configuration_efforts(&high_body), ["max", "high"]);
}
