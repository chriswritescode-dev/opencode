use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt;
use opencode_ai::ExecutionContext;
use opencode_ai::LlmClient;
use opencode_ai::MapEnv;
use opencode_ai::clock::FixedClock;
use opencode_ai::id::SequenceIds;
use opencode_ai::route::compile::StreamOptions;
use opencode_ai::route::executor::{PreparedHttp, RequestExecutor};
use opencode_ai::schema::{ContentPart, LlmRequest, Message, TextPart, UserMessage};
use opencode_catalog::builtin::Builtin;
use opencode_catalog::credential::{CreateInput, Update};
use opencode_catalog::integration::{Attempt, AttemptError, AttemptStatus, Implementation, OAuthConnectInput};
use opencode_catalog::provider_plugin::PluginContext;
use opencode_catalog::provider_plugin::hooks::{HttpResponse, RequestKind};
use opencode_catalog::provider_plugin::http::BufferedResponse;
use opencode_catalog::provider_plugin::opencode;
use opencode_catalog::resolver::{self, Resolver};
use opencode_config::entry::{Document, Entry, Origin};
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    AgentID, ConnectionInfo, ConnectionStatus, CredentialKey, CredentialKeyType, CredentialOAuth, CredentialOAuthType,
    CredentialValue, FormAnswer, FormField1, FormStringField1Format, FormStringField1Type, FormValue,
    IntegrationEnvMethod, IntegrationEnvMethodType, IntegrationID, IntegrationKeyMethodType, IntegrationMethod,
    IntegrationMethodID, IntegrationOAuthMethodType, ModelCost, ModelCostCache, ModelInfo, ModelRef, ModelSettings,
    ModelVariant, MoneyUSDPerMillionTokens, ProviderID, ProviderInfo, ProviderInfoActivation, SessionID,
};
use serde_json::{Value, json};

use crate::builtin::{Harness, Input, start};
use crate::provider_plugin::support::{
    Exchange, RecordedSender, Reply, Routes, headers, plugin_context_with, provider_settings_from, settings_json,
};

const ORIGIN: &str = "https://console.example.com";

const USER_AGENT: &str = "opencode/test/test/opencode";

fn integration_id() -> IntegrationID {
    IntegrationID("opencode".to_owned())
}

fn method_id() -> IntegrationMethodID {
    IntegrationMethodID("device".to_owned())
}

async fn add_plugin(harness: &mut Harness, http: Arc<dyn opencode_ai::HttpSender>) -> PluginContext {
    let context = plugin_context_with(harness, http);
    let mut builtin = Builtin::default();
    opencode::register(&harness.services, &context, &mut builtin).await;
    harness.record(builtin);
    context
}

fn console_document(harness: &Harness) -> Option<Document> {
    harness.services.config.entries().into_iter().find_map(|entry| match entry {
        Entry::Document(document) => match &document.origin {
            Origin::Integration { integration_id, .. } if integration_id == "opencode" => Some(document),
            _ => None,
        },
        Entry::Directory { .. } => None,
    })
}

fn connection_status(connection: &ConnectionInfo) -> Option<&ConnectionStatus> {
    match connection {
        ConnectionInfo::Credential(connection) => connection.status.as_ref(),
        ConnectionInfo::Env(connection) => connection.status.as_ref(),
    }
}

async fn integration_status(harness: &Harness) -> Option<ConnectionStatus> {
    harness
        .services
        .integration
        .get(&integration_id())
        .await
        .expect("get the OpenCode integration")
        .and_then(|integration| integration.connections.first().and_then(connection_status).cloned())
}

fn console_key(server: &str, organization: Option<(&str, &str)>) -> CredentialValue {
    console_key_with(server, "secret", organization)
}

fn console_key_with(server: &str, token: &str, organization: Option<(&str, &str)>) -> CredentialValue {
    let mut metadata = vec![("server".to_owned(), json!(server))];
    if let Some((id, name)) = organization {
        metadata.push(("orgID".to_owned(), json!(id)));
        metadata.push(("orgName".to_owned(), json!(name)));
    }
    CredentialValue::Key(CredentialKey {
        r#type: CredentialKeyType::Key,
        key: token.to_owned(),
        metadata: Some(JsonMap(metadata)),
    })
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

fn cost(input: f64, output: f64) -> ModelCost {
    ModelCost {
        tier: None,
        input: MoneyUSDPerMillionTokens(input),
        output: MoneyUSDPerMillionTokens(output),
        cache: ModelCostCache { read: MoneyUSDPerMillionTokens(0.0), write: MoneyUSDPerMillionTokens(0.0) },
    }
}

fn opencode_id() -> ProviderID {
    ProviderID("opencode".to_owned())
}

fn api_key(provider: &ProviderInfo) -> Option<String> {
    settings_json(provider).and_then(|settings| settings.get("apiKey").and_then(Value::as_str).map(str::to_owned))
}

async fn seed_provider(
    harness: &Harness,
    provider_id: ProviderID,
    configure: impl Fn(&mut ProviderInfo) + Send + Sync + 'static,
    models: Vec<(&str, f64, f64)>,
) -> opencode_location::state::Registration {
    let models: Vec<(String, f64, f64)> =
        models.into_iter().map(|(id, input, output)| (id.to_owned(), input, output)).collect();
    harness
        .services
        .provider
        .transform(move |editor| {
            editor.update(&provider_id, &configure);
            for (id, input, output) in &models {
                editor.models_update(&provider_id, id, |model| model.cost = vec![cost(*input, *output)]);
            }
            Ok(())
        })
        .await
}

async fn register_env_method(harness: &Harness, names: &[&str]) -> opencode_location::state::Registration {
    let names: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
    harness
        .services
        .integration
        .transform(move |editor| {
            editor.method_update(Implementation::Method {
                integration_id: integration_id(),
                method: IntegrationMethod::Env(IntegrationEnvMethod {
                    r#type: IntegrationEnvMethodType::Env,
                    names: names.clone(),
                }),
            });
            Ok(())
        })
        .await
}

fn no_remote_config() -> Arc<Routes> {
    let routes = Routes::new();
    routes.route_with("GET", &config_url(opencode_catalog::console_access::DEFAULT_SERVER), |_| {
        Reply::json_status(json!(null), 404)
    });
    routes
}

fn fresh_oauth() -> CredentialValue {
    CredentialValue::Oauth(CredentialOAuth {
        r#type: CredentialOAuthType::Oauth,
        method_id: method_id(),
        access: "access".to_owned(),
        refresh: "refresh".to_owned(),
        expires: 9_007_199_254_740_991.0,
        metadata: None,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uses_a_public_key_and_disables_paid_models_without_credentials() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = seed_provider(&harness, opencode_id(), |_| {}, vec![("paid", 1.0, 0.0)]).await;
    let routes = Routes::new();
    add_plugin(&mut harness, routes.clone()).await;
    assert!(routes.requests().is_empty(), "a location without credentials must not contact the console");

    let provider = harness.services.provider.get(&opencode_id()).await.unwrap().expect("the opencode provider");
    assert_eq!(api_key(&provider).as_deref(), Some("public"));
    let model = harness.services.model.get(&opencode_id(), "paid").await.unwrap().expect("the paid model");
    assert!(!model.enabled);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keeps_free_models_without_credentials() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = seed_provider(&harness, opencode_id(), |_| {}, vec![("free", 0.0, 0.0)]).await;
    add_plugin(&mut harness, Routes::new()).await;

    let provider = harness.services.provider.get(&opencode_id()).await.unwrap().expect("the opencode provider");
    assert_eq!(api_key(&provider).as_deref(), Some("public"));
    assert_eq!(provider.activation, ProviderInfoActivation::Enabled);
    assert!(harness.services.provider.available().await.unwrap().iter().any(|item| item.id == opencode_id()));
    let model = harness.services.model.get(&opencode_id(), "free").await.unwrap().expect("the free model");
    assert!(model.enabled);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn treats_output_only_cost_as_free_without_credentials() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = seed_provider(&harness, opencode_id(), |_| {}, vec![("output-only", 0.0, 1.0)]).await;
    add_plugin(&mut harness, Routes::new()).await;

    let provider = harness.services.provider.get(&opencode_id()).await.unwrap().expect("the opencode provider");
    assert_eq!(api_key(&provider).as_deref(), Some("public"));
    let model = harness.services.model.get(&opencode_id(), "output-only").await.unwrap().expect("the model");
    assert!(model.enabled);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uses_opencode_api_key_as_credentials() {
    let mut harness = start(Input::new(Vec::new()).environment("OPENCODE_API_KEY", "secret")).await;
    let _method = register_env_method(&harness, &["OPENCODE_API_KEY"]).await;
    let _seeded = seed_provider(&harness, opencode_id(), |_| {}, vec![("paid", 1.0, 0.0)]).await;
    add_plugin(&mut harness, no_remote_config()).await;

    let provider = harness.services.provider.get(&opencode_id()).await.unwrap().expect("the opencode provider");
    assert_eq!(api_key(&provider), None);
    let model = harness.services.model.get(&opencode_id(), "paid").await.unwrap().expect("the paid model");
    assert!(model.enabled);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uses_configured_provider_env_vars_as_credentials() {
    let mut harness = start(Input::new(Vec::new()).environment("CUSTOM_OPENCODE_API_KEY", "secret")).await;
    let _method = register_env_method(&harness, &["CUSTOM_OPENCODE_API_KEY"]).await;
    let _seeded = seed_provider(&harness, opencode_id(), |_| {}, vec![("paid", 1.0, 0.0)]).await;
    add_plugin(&mut harness, no_remote_config()).await;

    let provider = harness.services.provider.get(&opencode_id()).await.unwrap().expect("the opencode provider");
    assert_eq!(api_key(&provider), None);
    let model = harness.services.model.get(&opencode_id(), "paid").await.unwrap().expect("the paid model");
    assert!(model.enabled);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uses_configured_api_key_as_credentials() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = seed_provider(
        &harness,
        opencode_id(),
        |provider| {
            provider.package = "aisdk:test-provider".to_owned();
            provider.settings = Some(provider_settings_from(json!({ "apiKey": "configured" })));
            provider.activation = ProviderInfoActivation::Enabled;
        },
        vec![("paid", 1.0, 0.0)],
    )
    .await;
    add_plugin(&mut harness, Routes::new()).await;

    let provider = harness.services.provider.get(&opencode_id()).await.unwrap().expect("the opencode provider");
    assert_eq!(api_key(&provider).as_deref(), Some("configured"));
    let model = harness.services.model.get(&opencode_id(), "paid").await.unwrap().expect("the paid model");
    assert!(model.enabled);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ignores_non_opencode_providers_and_models() {
    let mut harness = start(Input::new(Vec::new())).await;
    let openai = ProviderID("openai".to_owned());
    let _seeded = seed_provider(&harness, openai.clone(), |_| {}, vec![("paid", 1.0, 0.0)]).await;
    add_plugin(&mut harness, Routes::new()).await;

    let provider = harness.services.provider.get(&openai).await.unwrap().expect("the openai provider");
    assert_eq!(api_key(&provider), None);
    let model = harness.services.model.get(&openai, "paid").await.unwrap().expect("the paid model");
    assert!(model.enabled);
}

async fn wait_for_opencode_models(harness: &Harness) -> Vec<ModelInfo> {
    for _ in 0..2000 {
        let models = harness.services.model.available(None).await.unwrap();
        if models.iter().any(|model| model.provider_id == opencode_id()) {
            return models;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("the Console login makes the OpenCode models available");
}

async fn disabled_free_models(api_key: Option<&str>) {
    let mut input = Input::new(Vec::new()).environment("OPENCODE_DISABLE_FREE_MODELS", "true");
    if let Some(api_key) = api_key {
        input = input.environment("OPENCODE_API_KEY", api_key);
    }
    let mut harness = start(input).await;
    let _method = register_env_method(&harness, &["OPENCODE_API_KEY"]).await;
    let _seeded = seed_provider(&harness, opencode_id(), |_| {}, vec![("free", 0.0, 0.0), ("paid", 1.0, 0.0)]).await;
    add_plugin(&mut harness, no_remote_config()).await;

    if api_key.is_none() {
        assert!(harness.services.provider.available().await.unwrap().is_empty());
        assert!(harness.services.model.available(None).await.unwrap().is_empty());
        assert!(harness.services.model.all().await.unwrap().is_empty());
        assert!(harness.services.model.default_model().await.unwrap().is_none());
        let integration =
            harness.services.integration.get(&integration_id()).await.unwrap().expect("the OpenCode integration");
        assert!(!integration.methods.is_empty());
        harness.credential("opencode", fresh_oauth()).await;
        wait_for_opencode_models(&harness).await;
    }

    let available = harness.services.model.available(None).await.unwrap();
    assert!(harness.services.provider.available().await.unwrap().iter().any(|item| item.id == opencode_id()));
    assert_eq!(available.iter().map(|model| model.id.as_str()).collect::<Vec<_>>(), ["paid"]);
    assert_eq!(harness.services.model.all().await.unwrap().len(), 1);
    assert_eq!(harness.services.model.default_model().await.unwrap().map(|model| model.id), Some("paid".to_owned()));
    assert!(harness.services.model.get(&opencode_id(), "free").await.unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_free_models_with_an_api_key() {
    disabled_free_models(Some("secret")).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_free_models_with_a_console_login_after_startup() {
    disabled_free_models(None).await;
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
    routes.route_json_with_headers("GET", &config_url(ORIGIN), lists_headers(), json!({ "providers": {} }));
    routes.route_json("GET", &list_url("instructions"), json!({ "instructions": [] }));
    routes.route_json("GET", &list_url("skills"), json!({ "skills": [] }));
    let mut harness = start(Input::new(Vec::new())).await;
    add_plugin(&mut harness, routes.clone()).await;

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

    advance_until(|| routes.count("GET", &config_url(ORIGIN)) > 0).await;
    advance_tick().await;
    assert_eq!(routes.count("GET", &config_url(ORIGIN)), 1);
    let config = request(&routes, "GET", &config_url(ORIGIN));
    assert_eq!(header(&config, "authorization").as_deref(), Some("Bearer access"));
    assert_eq!(header(&config, "x-org-id").as_deref(), Some(expected_org));
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
async fn picks_the_first_organization_by_locale_name_without_a_device_token_organization() {
    let routes = Routes::new();
    routes.route_json(
        "POST",
        &format!("{ORIGIN}/auth/device/code"),
        json!({
            "device_code": "device",
            "user_code": "user",
            "verification_uri_complete": "/device?user_code=user",
            "expires_in": 60,
            "interval": 0,
        }),
    );
    routes.route_json(
        "POST",
        &format!("{ORIGIN}/auth/device/token"),
        json!({ "access_token": "access", "refresh_token": "refresh", "expires_in": 600 }),
    );
    routes.route_json("GET", &format!("{ORIGIN}/api/user"), json!({ "id": "user", "email": "user@example.com" }));
    routes.route_json(
        "GET",
        &format!("{ORIGIN}/api/orgs"),
        json!([{ "id": "org-b", "name": "Beta" }, { "id": "org-a", "name": "acme" }]),
    );
    let mut harness = start(Input::new(Vec::new())).await;
    add_plugin(&mut harness, routes).await;

    let attempt =
        connect(&harness, answer(FormValue::String(ORIGIN.to_owned()))).await.expect("start the device authorization");
    let status = wait_for_status(&harness, &attempt).await;
    assert!(matches!(status, AttemptStatus::Complete { .. }), "got {status:?}");

    let credentials = harness.services.credentials.list(&integration_id()).expect("list the OpenCode credentials");
    let CredentialValue::Oauth(value) = &credentials[0].value else { panic!("the OpenCode credential is OAuth") };
    assert_eq!(metadata(value, "orgID"), Some(&json!("org-a")));
    assert_eq!(metadata(value, "orgName"), Some(&json!("acme")));
}

#[tokio::test(start_paused = true)]
async fn reports_a_missing_device_token_organization() {
    let routes = console_routes(ORIGIN, token(Some(json!("org-missing"))));
    routes.route_json_with_headers("GET", &config_url(ORIGIN), lists_headers(), json!({ "providers": {} }));
    let mut harness = start(Input::new(Vec::new())).await;
    add_plugin(&mut harness, routes.clone()).await;

    let attempt =
        connect(&harness, answer(FormValue::String(ORIGIN.to_owned()))).await.expect("start the device authorization");
    let status = wait_for_status(&harness, &attempt).await;
    let AttemptStatus::Failed { message, .. } = status else { panic!("the attempt fails, got {status:?}") };
    assert_eq!(message, "OpenCode organization not found: org-missing");
    assert!(harness.services.credentials.list(&integration_id()).expect("list the OpenCode credentials").is_empty());
    advance_tick().await;
    assert_eq!(routes.count("GET", &config_url(ORIGIN)), 0);
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

async fn rotates_a_stored_credential(org_id: Option<Value>, expected: (&str, &str), unavailable: bool) {
    let routes = Routes::new();
    routes.route_json("POST", &format!("{ORIGIN}/auth/device/token"), token(org_id));
    routes.route_json_with_headers("GET", &config_url(ORIGIN), lists_headers(), json!({ "providers": {} }));
    routes.route_json("GET", &list_url("instructions"), json!({ "instructions": [] }));
    routes.route_json("GET", &list_url("skills"), json!({ "skills": [] }));
    if unavailable {
        routes.route_with("GET", &format!("{ORIGIN}/api/user"), |_| Reply::status(503));
        routes.route_with("GET", &format!("{ORIGIN}/api/orgs"), |_| Reply::status(503));
    }
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

    let requests = eventually_console_requests(&routes).await;
    assert_eq!(
        requests.iter().map(|request| request.url.clone()).collect::<Vec<_>>(),
        [format!("{ORIGIN}/auth/device/token"), config_url(ORIGIN), list_url("instructions"), list_url("skills"),]
    );
    let agents: Vec<Option<String>> = requests.iter().map(|request| header(request, "user-agent")).collect();
    assert!(agents.iter().all(|agent| agent.as_deref() == Some(USER_AGENT)), "got {agents:?}");
    let config = request(&routes, "GET", &config_url(ORIGIN));
    assert_eq!(header(&config, "authorization").as_deref(), Some("Bearer access"));
    assert_eq!(header(&config, "x-org-id").as_deref(), Some(expected.0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn persists_rotated_credentials() {
    for (org_id, expected, unavailable) in [
        (Some(json!("org-z")), ("org-z", "org-z"), false),
        (None, ("org-a", "Alpha"), false),
        (Some(json!(null)), ("org-a", "Alpha"), false),
        (Some(json!("org-missing")), ("org-missing", "org-missing"), false),
        (Some(json!("org-z")), ("org-z", "org-z"), true),
        (Some(json!("org-a")), ("org-a", "Alpha"), true),
    ] {
        rotates_a_stored_credential(org_id, expected, unavailable).await;
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

fn config_url(server: &str) -> String {
    format!("{server}/api/v2/config")
}

fn encoded(value: &impl opencode_contract::codec::Encode) -> Value {
    serde_json::from_str(&opencode_contract::codec::encode_json(value)).expect("the value encodes as JSON")
}

fn document_json(document: &Document) -> Value {
    encoded(&document.info)
}

fn document_policies(document: &Document) -> Value {
    document_json(document).get("policies").cloned().unwrap_or(Value::Null)
}

fn document_organization(document: &Document) -> Option<&str> {
    match &document.origin {
        Origin::Integration { organization, .. } => organization.as_deref(),
        _ => None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn merges_the_console_response_as_config_with_its_policy_named_after_the_organization() {
    let routes = Routes::new();
    routes.route_json(
        "GET",
        &config_url(ORIGIN),
        json!({
            "$schema": "https://opencode.ai/config.json",
            "providers": { "remote": { "name": "Remote" } },
            "policies": [{ "action": "tool.use", "resource": "shell:*", "effect": "deny" }],
        }),
    );
    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential("opencode", console_key(ORIGIN, Some(("org_acme", "Acme")))).await;
    add_plugin(&mut harness, routes.clone()).await;

    let document = console_document(&harness).expect("the Console document is integrated");
    assert_eq!(document_json(&document)["providers"]["remote"]["name"], json!("Remote"));
    assert_eq!(document_organization(&document), Some("Acme"));
    assert_eq!(
        document_policies(&document),
        json!([{ "action": "tool.use", "resource": "shell:*", "effect": "deny" }])
    );

    let request = request(&routes, "GET", &config_url(ORIGIN));
    assert_eq!(header(&request, "authorization").as_deref(), Some("Bearer secret"));
    assert_eq!(header(&request, "x-org-id").as_deref(), Some("org_acme"));
    assert_eq!(header(&request, "accept").as_deref(), Some("application/json"));
    assert_eq!(header(&request, "user-agent").as_deref(), Some(USER_AGENT));
    assert_eq!(routes.count("GET", &config_url(ORIGIN)), 1);
    assert!(integration_status(&harness).await.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn passes_only_providers_remote_mcp_servers_policies_and_the_web_search_selection_without_credentials() {
    let routes = Routes::new();
    routes.route_json(
        "GET",
        &config_url(ORIGIN),
        json!({
            "$schema": "https://opencode.ai/config.json",
            "providers": {
                "remote": {
                    "env": ["REMOTE_API_KEY"],
                    "settings": { "apiKey": "key", "authToken": "auth", "accessToken": "access", "baseURL": "https://remote" },
                    "models": {
                        "chat": {
                            "settings": { "apiKey": "key", "temperature": 0.1 },
                            "variants": [{ "id": "high", "settings": { "accessToken": "access", "temperature": 0.2 } }],
                        },
                    },
                },
            },
            "mcp": {
                "servers": {
                    "remote": { "type": "remote", "url": "https://remote/mcp" },
                    "local": { "type": "local", "command": ["/tmp/console-mcp"] },
                },
            },
            "policies": [{ "action": "tool.use", "resource": "shell:*", "effect": "deny" }],
            "websearch": { "provider": "opencode" },
            "plugins": ["file:///tmp/console-plugin"],
            "shell": "/tmp/console-shell",
            "agents": { "console": { "prompt": "Console agent" } },
            "commands": { "console": { "template": "Console command" } },
            "permissions": [{ "action": "shell", "resource": "*", "effect": "allow" }],
            "formatter": { "console": { "command": ["/tmp/console-formatter"] } },
            "instructions": ["/tmp/console-instructions.md"],
        }),
    );
    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential("opencode", console_key(ORIGIN, None)).await;
    add_plugin(&mut harness, routes).await;

    let document = console_document(&harness).expect("the Console document is integrated");
    let info = document_json(&document);
    let mut keys: Vec<&str> = info.as_object().expect("the document is an object").keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["mcp", "policies", "providers", "websearch"]);
    let servers = info["mcp"]["servers"].as_object().expect("the MCP servers are an object");
    assert_eq!(servers.keys().map(String::as_str).collect::<Vec<_>>(), ["remote"]);
    assert_eq!(info["providers"]["remote"].get("env"), None);
    assert_eq!(info["providers"]["remote"]["settings"], json!({ "baseURL": "https://remote" }));
    assert_eq!(info["providers"]["remote"]["models"]["chat"]["settings"], json!({ "temperature": 0.1 }));
    assert_eq!(info["providers"]["remote"]["models"]["chat"]["variants"][0]["settings"], json!({ "temperature": 0.2 }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keeps_a_newer_console_config_loadable_despite_values_this_version_does_not_know() {
    let routes = Routes::new();
    routes.route_json(
        "GET",
        &config_url(ORIGIN),
        json!({
            "$schema": "https://opencode.ai/config.json",
            "providers": { "remote": { "name": "Remote" } },
            "policies": [
                { "action": "future.use", "resource": "anything", "effect": "deny" },
                { "action": "tool.use", "resource": "shell:*", "effect": "deny" },
            ],
            "shell": 123,
            "future": { "shape": 42 },
        }),
    );
    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential("opencode", console_key(ORIGIN, None)).await;
    add_plugin(&mut harness, routes).await;

    let document = console_document(&harness).expect("the Console document is integrated");
    assert_eq!(document_json(&document)["providers"]["remote"]["name"], json!("Remote"));
    assert_eq!(
        document_policies(&document),
        json!([{ "action": "tool.use", "resource": "shell:*", "effect": "deny" }])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sends_credentials_saved_with_the_retired_console_opencode_ai_host_to_the_default_console() {
    let routes = Routes::new();
    routes.route_json("GET", &config_url("https://opencode.ai/console"), json!({}));
    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential("opencode", console_key("https://console.opencode.ai", None)).await;
    add_plugin(&mut harness, routes.clone()).await;

    let requests: Vec<String> = routes.requests().into_iter().map(|request| request.url).collect();
    assert_eq!(requests, [config_url("https://opencode.ai/console")]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn releases_unread_console_config_responses_when_the_fetch_completes() {
    let routes = Routes::new();
    routes.route_with("GET", &config_url("https://console.test"), |_| Reply::json_status(json!(null), 404));
    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential("opencode", console_key("https://console.test", None)).await;
    add_plugin(&mut harness, routes.clone()).await;

    assert_eq!(routes.count("GET", &config_url("https://console.test")), 1);
    assert!(console_document(&harness).is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_console_sso_sign_in_keeps_the_last_config_and_clears_once_console_accepts_again() {
    let routes = Routes::new();
    let sudo = json!({ "action": "tool.use", "resource": "shell:sudo *", "effect": "deny" });
    routes.route_json("GET", &config_url(ORIGIN), json!({ "providers": {}, "policies": [sudo.clone()] }));
    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential("opencode", console_key(ORIGIN, Some(("org_acme", "Acme")))).await;
    let context = add_plugin(&mut harness, routes.clone()).await;
    assert!(integration_status(&harness).await.is_none());
    assert_eq!(context.console.allowed(), opencode::Allowed::default());
    let changes = context.console.changes();

    routes.remove("GET", &config_url(ORIGIN));
    routes.route_with("GET", &config_url(ORIGIN), |_| {
        Reply::json_status(json!({ "_tag": "SsoRequired", "orgId": "org_acme", "connectionId": "ssoconn/1" }), 403)
    });
    opencode::refresh(&harness.services, &context, Duration::ZERO).await;
    assert!(changes.has_changed().expect("the Console revision"));
    assert_eq!(
        integration_status(&harness).await,
        Some(ConnectionStatus {
            status: opencode_contract::generated::ConnectionStatusStatus::NeedsAuth,
            message: "Sign in with SSO again to use Acme".to_owned(),
            url: Some(format!("{ORIGIN}/auth/sso/ssoconn%2F1/start?redirectTo=%2F")),
        })
    );
    let document = console_document(&harness).expect("the last Console document is kept");
    assert_eq!(document_policies(&document), json!([sudo.clone()]));

    routes.remove("GET", &config_url(ORIGIN));
    routes.route_json("GET", &config_url(ORIGIN), json!({ "providers": {}, "policies": [sudo.clone()] }));
    opencode::refresh(&harness.services, &context, Duration::ZERO).await;
    assert!(integration_status(&harness).await.is_none());
    let document = console_document(&harness).expect("the Console document is kept");
    assert_eq!(document_policies(&document), json!([sudo]));
}

fn policy_list(document: &Document) -> Value {
    match document_policies(document) {
        Value::Null => json!([]),
        policies => policies,
    }
}

fn console_response(provider_id: &str, status: u16, body: &Value) -> HttpResponse {
    HttpResponse {
        session_id: SessionID("ses_console".to_owned()),
        agent: AgentID("build".to_owned()),
        model: ModelRef { id: "model".to_owned(), provider_id: ProviderID(provider_id.to_owned()), variant: None },
        kind: RequestKind::Primary,
        request: PreparedHttp::new("POST", "https://inference.example/v1/responses", Vec::new(), Bytes::new()),
        response: BufferedResponse {
            status,
            headers: vec![("content-type".to_owned(), "application/json".to_owned())],
            body: Bytes::from(serde_json::to_vec(body).expect("the response body serializes")),
        },
    }
}

fn response_json(response: &HttpResponse) -> Value {
    serde_json::from_slice(&response.response.body).expect("the response body is JSON")
}

async fn advance_tick() {
    tokio::time::advance(Duration::from_millis(500)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

async fn advance_until(mut condition: impl FnMut() -> bool) {
    for _ in 0..400 {
        if condition() {
            return;
        }
        advance_tick().await;
    }
    panic!("the condition was not reached");
}

async fn advance_for(duration: Duration) {
    for _ in 0..duration.as_millis().div_ceil(500) {
        advance_tick().await;
    }
}

fn account_config(organization: &str) -> Value {
    let providers = serde_json::Map::from_iter([(organization.to_owned(), json!({}))]);
    let servers = serde_json::Map::from_iter([(
        organization.to_owned(),
        json!({ "type": "remote", "url": format!("https://{organization}/mcp") }),
    )]);
    json!({
        "providers": Value::Object(providers),
        "mcp": { "servers": Value::Object(servers) },
        "policies": [{ "action": "tool.use", "resource": format!("shell:{organization}"), "effect": "deny" }],
        "websearch": { "provider": "opencode" },
    })
}

fn console_contents(harness: &Harness) -> Option<Value> {
    let document = console_document(harness)?;
    let info = document_json(&document);
    let keys = |value: Option<&Value>| {
        value.and_then(Value::as_object).map(|map| map.keys().cloned().collect::<Vec<_>>()).unwrap_or_default()
    };
    Some(json!({
        "organization": document_organization(&document),
        "providers": keys(info.get("providers")),
        "mcp": keys(info.get("mcp").and_then(|mcp| mcp.get("servers"))),
        "policies": info
            .get("policies")
            .and_then(Value::as_array)
            .map(|policies| policies.iter().map(|policy| policy["resource"].clone()).collect::<Vec<_>>())
            .unwrap_or_default(),
        "websearch": info.get("websearch").and_then(|websearch| websearch.get("provider")).cloned(),
    }))
}

fn expected_contents(organization: &str) -> Value {
    json!({
        "organization": organization,
        "providers": [organization],
        "mcp": [organization],
        "policies": [format!("shell:{organization}")],
        "websearch": "opencode",
    })
}

async fn eventually_contents(harness: &Harness, expected: &Value) -> Value {
    for _ in 0..2000 {
        if let Some(value) = console_contents(harness)
            && &value == expected
        {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("the Console config did not become {expected}");
}

async fn eventually_console_none(harness: &Harness) {
    for _ in 0..2000 {
        if console_document(harness).is_none() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("the Console config was not cleared");
}

#[tokio::test(start_paused = true)]
async fn refreshes_past_future_only_policies_while_retaining_config_on_malformed_statements() {
    let sudo = json!({ "action": "tool.use", "resource": "shell:sudo *", "effect": "deny" });
    let env = json!({ "action": "tool.use", "resource": "edit:*.env", "effect": "deny" });
    let state = Arc::new(Mutex::new(json!({
        "providers": { "opencode": {} },
        "policies": [sudo.clone()],
    })));
    let routes = Routes::new();
    routes.route_with("GET", &config_url(ORIGIN), {
        let state = state.clone();
        move |_| Reply::json(state.lock().expect("lock the Console config state").clone())
    });
    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential("opencode", console_key(ORIGIN, Some(("org_acme", "Acme")))).await;
    add_plugin(&mut harness, routes.clone()).await;

    let document = console_document(&harness).expect("the Console document is integrated");
    assert_eq!(policy_list(&document), json!([sudo.clone()]));

    for malformed in [
        json!({ "action": "tool.use", "resource": 42, "effect": "deny" }),
        json!({ "action": "tool.use", "effect": "deny" }),
        Value::Null,
        json!({}),
    ] {
        *state.lock().expect("lock the Console config state") =
            json!({ "providers": { "opencode": {} }, "policies": [env.clone(), malformed] });
        let before = routes.count("GET", &config_url(ORIGIN));
        advance_until(|| routes.count("GET", &config_url(ORIGIN)) > before).await;
        advance_tick().await;
        advance_tick().await;
        let document = console_document(&harness).expect("the last Console document is kept");
        assert_eq!(policy_list(&document), json!([sudo.clone()]));
        assert!(harness.services.provider.get(&opencode_id()).await.unwrap().is_some());
    }

    *state.lock().expect("lock the Console config state") = json!({
        "providers": { "opencode": {} },
        "policies": [env.clone(), { "action": "tool.use", "resource": "shell:*", "effect": "future-effect" }],
    });
    let before = routes.count("GET", &config_url(ORIGIN));
    advance_until(|| routes.count("GET", &config_url(ORIGIN)) > before).await;
    advance_tick().await;
    advance_tick().await;
    let document = console_document(&harness).expect("the Console document is integrated");
    assert_eq!(policy_list(&document), json!([env.clone()]));

    *state.lock().expect("lock the Console config state") = json!({ "providers": { "opencode": {} }, "policies": [{ "action": "future.use", "future": { "values": [true] } }] });
    let before = routes.count("GET", &config_url(ORIGIN));
    advance_until(|| routes.count("GET", &config_url(ORIGIN)) > before).await;
    advance_tick().await;
    advance_tick().await;
    let document = console_document(&harness).expect("the Console document is integrated");
    assert_eq!(policy_list(&document), json!([]));
    assert!(harness.services.provider.get(&opencode_id()).await.unwrap().is_some());

    *state.lock().expect("lock the Console config state") = json!({ "providers": { "opencode": {} }, "policies": [{ "action": "future.use", "effect": { "next": true } }, env] });
    let before = routes.count("GET", &config_url(ORIGIN));
    advance_until(|| routes.count("GET", &config_url(ORIGIN)) > before).await;
    advance_tick().await;
    advance_tick().await;
    let document = console_document(&harness).expect("the Console document is integrated");
    assert_eq!(policy_list(&document), json!([env]));
}

#[tokio::test(start_paused = true)]
async fn explains_rejected_console_inference_from_at_most_one_config_check_per_30_seconds() {
    let sso = Arc::new(Mutex::new(false));
    let routes = Routes::new();
    routes.route_with("GET", &config_url(ORIGIN), {
        let sso = sso.clone();
        move |_| {
            if *sso.lock().expect("lock the SSO state") {
                Reply::json_status(
                    json!({ "_tag": "SsoRequired", "orgId": "org_acme", "connectionId": "ssoconn_1" }),
                    403,
                )
            } else {
                Reply::json(json!({ "providers": { "remote": { "canonical": "openai" } } }))
            }
        }
    });
    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential("opencode", console_key(ORIGIN, Some(("org_acme", "Acme")))).await;
    let context = add_plugin(&mut harness, routes.clone()).await;
    assert_eq!(routes.count("GET", &config_url(ORIGIN)), 1);

    let denied = json!({ "error": { "message": "Workspace access denied" } });
    let explained =
        json!({ "error": { "type": "authentication_error", "message": "Sign in with SSO again to use Acme." } });

    let event = context.hooks.trigger_http_response(console_response("remote", 403, &denied)).await;
    assert_eq!(event.response.status, 403);
    assert_eq!(response_json(&event), denied);
    assert_eq!(routes.count("GET", &config_url(ORIGIN)), 1);

    *sso.lock().expect("lock the SSO state") = true;
    tokio::time::advance(Duration::from_secs(30)).await;
    let events = futures_util::future::join_all(
        (0..10).map(|_| context.hooks.trigger_http_response(console_response("remote", 403, &denied))),
    )
    .await;
    for event in &events {
        assert_eq!(event.response.status, 403);
        assert_eq!(response_json(event), explained);
    }
    assert_eq!(routes.count("GET", &config_url(ORIGIN)), 2);

    for _ in 0..20 {
        tokio::time::advance(Duration::from_secs(1)).await;
        let event = context.hooks.trigger_http_response(console_response("remote", 403, &denied)).await;
        assert_eq!(response_json(&event), explained);
    }
    let event = context.hooks.trigger_http_response(console_response("opencode", 403, &denied)).await;
    assert_eq!(response_json(&event), explained);
    let event = context.hooks.trigger_http_response(console_response("anthropic", 403, &denied)).await;
    assert_eq!(response_json(&event), denied);
    assert_eq!(routes.count("GET", &config_url(ORIGIN)), 2);
}

#[tokio::test(start_paused = true)]
async fn ignores_403_responses_that_are_not_a_console_sso_error() {
    type ReplyBody = Arc<Mutex<Option<(u16, Vec<u8>)>>>;

    let sudo = json!({ "action": "tool.use", "resource": "shell:sudo *", "effect": "deny" });
    let current: ReplyBody = Arc::new(Mutex::new(None));
    let routes = Routes::new();
    routes.route_with("GET", &config_url(ORIGIN), {
        let current = current.clone();
        let sudo = sudo.clone();
        move |_| match current.lock().expect("lock the Console response").clone() {
            Some((status, body)) => Reply { status, body },
            None => Reply::json(json!({ "providers": {}, "policies": [sudo.clone()] })),
        }
    });
    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential("opencode", console_key(ORIGIN, None)).await;
    add_plugin(&mut harness, routes.clone()).await;

    for body in [
        b"<html>Forbidden</html>".to_vec(),
        serde_json::to_vec(&json!({ "_tag": "Forbidden", "orgId": "org", "connectionId": "ssoconn" }))
            .expect("the forbidden body serializes"),
    ] {
        *current.lock().expect("lock the Console response") = Some((403, body));
        let before = routes.count("GET", &config_url(ORIGIN));
        advance_until(|| routes.count("GET", &config_url(ORIGIN)) > before).await;
        advance_for(Duration::from_secs(10)).await;
        assert!(integration_status(&harness).await.is_none());
        let document = console_document(&harness).expect("the last Console document is kept");
        assert_eq!(policy_list(&document), json!([sudo.clone()]));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replaces_the_console_config_on_an_account_switch_and_clears_it_on_sign_out() {
    let routes = Routes::new();
    routes.route_with("GET", &config_url(ORIGIN), |request| {
        let organization = header(request, "x-org-id").unwrap_or_else(|| "none".to_owned());
        Reply::json(account_config(&organization))
    });
    let mut harness = start(Input::new(Vec::new())).await;
    let alpha = harness
        .services
        .credentials
        .create(CreateInput {
            id: None,
            integration_id: integration_id(),
            value: console_key(ORIGIN, Some(("alpha", "alpha"))),
            label: None,
            activate: None,
        })
        .expect("create the alpha Console account");
    add_plugin(&mut harness, routes).await;
    assert_eq!(console_contents(&harness), Some(expected_contents("alpha")));

    let beta = harness
        .services
        .credentials
        .create(CreateInput {
            id: None,
            integration_id: integration_id(),
            value: console_key(ORIGIN, Some(("beta", "beta"))),
            label: None,
            activate: None,
        })
        .expect("create the beta Console account");
    assert_eq!(eventually_contents(&harness, &expected_contents("beta")).await, expected_contents("beta"));

    harness.services.credentials.remove(&beta.id).expect("remove the beta Console account");
    assert_eq!(eventually_contents(&harness, &expected_contents("alpha")).await, expected_contents("alpha"));

    harness.services.credentials.remove(&alpha.id).expect("remove the alpha Console account");
    eventually_console_none(&harness).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preserves_native_console_openai_variant_bodies_in_inference_requests() {
    let variants = json!([
        { "id": "effort-only", "body": { "reasoning": { "effort": "low" } } },
        { "id": "summary-only", "body": { "reasoning": { "summary": "detailed" } } },
        { "id": "native", "body": { "reasoning": { "effort": "high", "summary": "auto" } } },
        { "id": "plain", "body": {} },
    ]);
    let models = json!({
        "astra": {
            "modelID": "api-astra",
            "variants": variants.as_array().expect("the variants are an array").iter().map(|variant| {
                let id = variant["id"].as_str().expect("the variant id");
                let mut body = serde_json::Map::new();
                if let Some(reasoning) = variant["body"].get("reasoning") {
                    body.insert("reasoning".to_owned(), reasoning.clone());
                }
                body.insert("include".to_owned(), json!(["reasoning.encrypted_content"]));
                body.insert("metadata".to_owned(), json!({ "custom": "unchanged" }));
                json!({
                    "id": id,
                    "headers": { "x-variant": id },
                    "body": Value::Object(body),
                })
            }).collect::<Vec<_>>(),
        },
    });
    let config = json!({
        "providers": {
            "remote": {
                "canonical": "openai",
                "package": "aisdk:@ai-sdk/openai",
                "settings": { "baseURL": ORIGIN },
                "models": {
                    "astra": models["astra"].clone(),
                    "override": {
                        "modelID": "api-astra",
                        "package": "aisdk:@ai-sdk/openai-compatible",
                        "variants": models["astra"]["variants"].clone(),
                    },
                },
            },
            "compatible": { "canonical": "openai", "package": "aisdk:@ai-sdk/openai-compatible", "models": models },
        },
    });
    let sse = "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_test\"}}\n\n";
    let mut exchanges =
        vec![Exchange::new("GET", &config_url(ORIGIN), 200, serde_json::to_vec(&config).expect("json"))];
    for _ in 0..4 {
        let mut response = Exchange::new("POST", &format!("{ORIGIN}/responses"), 200, sse.as_bytes());
        response.headers.push(("content-type".to_owned(), "text/event-stream".to_owned()));
        exchanges.push(response);
    }
    let sender = Arc::new(RecordedSender::keyed(exchanges));
    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential("opencode", console_key(ORIGIN, None)).await;
    add_plugin(&mut harness, sender.clone()).await;

    let remote = ProviderID("remote".to_owned());
    let model = harness.services.model.get(&remote, "astra").await.expect("read models").expect("the astra model");
    assert_eq!(model.canonical, Some(ProviderID("openai".to_owned())));
    let resolver = Resolver::new(
        harness.services.provider.clone(),
        harness.services.model.clone(),
        harness.services.integration.clone(),
        Arc::new(MapEnv::default()),
        Arc::new(resolver::NoBridge),
    );
    let clock = Arc::new(FixedClock::new(0));
    let client = LlmClient::new(
        RequestExecutor::new(sender.clone(), None, clock.clone()),
        ExecutionContext::new(clock, Arc::new(SequenceIds::new()), Arc::new(MapEnv::default())),
    );
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
        ..Default::default()
    };
    for variant in variants.as_array().expect("the variants are an array") {
        let id = variant["id"].as_str().expect("the variant id");
        let projected = model.variants.iter().find(|item| item.id == id).expect("the projected variant");
        let expected = json!({
            "reasoning": variant["body"].get("reasoning").cloned().unwrap_or(Value::Null),
            "include": ["reasoning.encrypted_content"],
            "metadata": { "custom": "unchanged" },
        });
        let expected = match expected.get("reasoning") {
            Some(Value::Null) => {
                let mut expected = expected;
                expected.as_object_mut().expect("the variant body").remove("reasoning");
                expected
            }
            _ => expected,
        };
        assert_eq!(encoded(projected.body.as_ref().expect("the variant body")), expected);
        assert!(projected.settings.is_none());

        let resolved = resolver.resolve_model(&model, Some(id)).await.expect("resolve the variant");
        let _ = client.stream(&resolved.model, request.clone(), StreamOptions::default()).collect::<Vec<_>>().await;
        let requests = sender.requests();
        let sent = requests
            .iter()
            .rev()
            .find(|request| request.method == "POST" && request.url == format!("{ORIGIN}/responses"))
            .expect("the inference request");
        assert_eq!(header(sent, "x-variant").as_deref(), Some(id));
        let body: Value = serde_json::from_slice(&sent.body).expect("the inference request body");
        assert_eq!(body.get("model"), Some(&json!("api-astra")));
        assert_eq!(body.get("include"), Some(&json!(["reasoning.encrypted_content"])));
        assert_eq!(body.get("metadata"), Some(&json!({ "custom": "unchanged" })));
        match variant["body"].get("reasoning") {
            Some(reasoning) => assert_eq!(body.get("reasoning"), Some(reasoning)),
            None => assert!(body.get("reasoning").is_none()),
        }
        assert!(body.get("reasoningEffort").is_none());
        assert!(body.get("reasoningSummary").is_none());
    }
    for (provider, model_id) in [("compatible", "astra"), ("remote", "override")] {
        let current = harness
            .services
            .model
            .get(&ProviderID(provider.to_owned()), model_id)
            .await
            .expect("read models")
            .expect("the model");
        let native = current.variants.iter().find(|variant| variant.id == "native").expect("the native variant");
        assert_eq!(
            encoded(native.body.as_ref().expect("the native body")),
            json!({
                "reasoning": { "effort": "high", "summary": "auto" },
                "include": ["reasoning.encrypted_content"],
                "metadata": { "custom": "unchanged" },
            })
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn layers_console_providers_below_user_config_bound_to_the_console_connection() {
    let routes = Routes::new();
    routes.route_json(
        "GET",
        &config_url(ORIGIN),
        json!({
            "providers": {
                "remote": {
                    "name": "Console",
                    "package": "aisdk:@ai-sdk/openai-compatible",
                    "settings": { "baseURL": format!("{ORIGIN}/v1"), "apiKey": "console-key", "custom": "console" },
                    "headers": { "x-org-id": "console", "x-console": "true" },
                    "models": {
                        "chat": { "name": "Console Chat", "settings": { "temperature": 0.1 } },
                        "other": { "name": "Console Other" },
                    },
                },
            },
        }),
    );
    let user = json!({
        "providers": {
            "remote": {
                "name": "Mine",
                "settings": { "apiKey": "my-key" },
                "headers": { "x-org-id": "mine" },
                "models": { "chat": { "name": "My Chat" } },
            },
        },
    });
    let mut harness = start(Input::new(vec![user])).await;
    harness.credential("opencode", console_key(ORIGIN, None)).await;
    add_plugin(&mut harness, routes).await;
    harness.register(&["config_provider"]).await;

    let remote = ProviderID("remote".to_owned());
    let provider = harness.services.provider.get(&remote).await.expect("read providers").expect("the remote provider");
    assert_eq!(provider.name, "Mine");
    assert_eq!(provider.integration_id, Some(IntegrationID("opencode".to_owned())));
    assert_eq!(
        headers(&provider),
        Some(
            [("x-org-id".to_owned(), "mine".to_owned()), ("x-console".to_owned(), "true".to_owned())]
                .into_iter()
                .collect()
        )
    );
    assert_eq!(
        settings_json(&provider),
        Some(json!({
            "baseURL": format!("{ORIGIN}/v1"),
            "custom": "console",
            "apiKey": "my-key",
            "provider": "remote",
        }))
    );
    let chat = harness.services.model.get(&remote, "chat").await.expect("read models").expect("the chat model");
    assert_eq!(chat.name, "My Chat");
    let chat_settings = encoded(chat.settings.as_ref().expect("the chat settings"));
    assert_eq!(chat_settings.get("temperature"), Some(&json!(0.1)));
    let other = harness.services.model.get(&remote, "other").await.expect("read models").expect("the other model");
    assert_eq!(other.name, "Console Other");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn applies_console_providers_before_provider_plugins_tune_their_canonical_provider() {
    let routes = Routes::new();
    routes.route_json(
        "GET",
        &config_url(ORIGIN),
        json!({ "providers": { "console-openai": { "canonical": "openai", "name": "Console OpenAI" } } }),
    );
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = harness
        .services
        .provider
        .transform(|editor| {
            editor.update(&ProviderID("openai".to_owned()), |provider| {
                provider.package = "aisdk:@ai-sdk/openai".to_owned();
            });
            Ok(())
        })
        .await;
    harness.credential("opencode", console_key(ORIGIN, None)).await;
    let context = add_plugin(&mut harness, routes).await;
    let mut builtin = Builtin::default();
    opencode_catalog::provider_plugin::openai::register(&harness.services, &context, &mut builtin).await;
    harness.record(builtin);
    harness.register(&["config_provider"]).await;

    let openai = harness
        .services
        .provider
        .get(&ProviderID("openai".to_owned()))
        .await
        .expect("read providers")
        .expect("the openai provider");
    assert_eq!(
        settings_json(&openai).and_then(|settings| settings.get("transport").cloned()),
        Some(json!("websocket"))
    );
    let console = harness
        .services
        .provider
        .get(&ProviderID("console-openai".to_owned()))
        .await
        .expect("read providers")
        .expect("the console openai provider");
    assert_eq!(console.name, "Console OpenAI");
    assert_eq!(console.integration_id, Some(IntegrationID("opencode".to_owned())));
    assert_eq!(settings_json(&console).and_then(|settings| settings.get("transport").cloned()), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn builds_console_reasoning_variants_after_provider_and_model_package_normalization() {
    let routes = Routes::new();
    routes.route_json(
        "GET",
        &config_url(ORIGIN),
        json!({
            "providers": {
                "remote": {
                    "name": "Remote",
                    "package": "aisdk:@ai-sdk/openai",
                    "models": {
                        "reasoner": {
                            "releaseDate": "2026-02-03",
                            "capabilities": { "reasoning": { "supported": true, "effort": ["default", "low", "high"] } },
                            "variants": [{ "id": "high", "headers": { "x-tier": "high" } }],
                        },
                        "gpt": {
                            "package": "aisdk:@ai-sdk/openai",
                            "capabilities": { "reasoning": { "supported": true, "effort": ["low", "high"] } },
                            "variants": [{ "id": "high", "headers": { "x-tier": "high" } }],
                        },
                        "claude-sonnet-4-5": {
                            "package": "aisdk:@ai-sdk/anthropic",
                            "capabilities": { "reasoning": { "supported": true, "budget": { "min": 1024 } } },
                            "limit": { "output": 64000 },
                        },
                        "gemini-3.1-pro": {
                            "package": "aisdk:@ai-sdk/google",
                            "capabilities": { "reasoning": { "supported": true, "effort": ["low", "high"] } },
                        },
                        "magistral-medium": {
                            "package": "aisdk:@ai-sdk/mistral",
                            "capabilities": { "reasoning": { "supported": true, "effort": ["low", "high"] } },
                        },
                    },
                },
            },
        }),
    );
    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential("opencode", console_key(ORIGIN, None)).await;
    add_plugin(&mut harness, routes).await;

    let remote = ProviderID("remote".to_owned());
    let reasoner = harness.services.model.get(&remote, "reasoner").await.expect("read models").expect("the reasoner");
    assert_eq!(reasoner.time.released, 1_770_076_800_000.0);
    assert_eq!(
        encoded(&reasoner.variants),
        json!([
            {
                "id": "low",
                "settings": { "reasoningEffort": "low", "reasoningSummary": "auto", "include": ["reasoning.encrypted_content"] },
            },
            {
                "id": "high",
                "settings": { "reasoningEffort": "high", "reasoningSummary": "auto", "include": ["reasoning.encrypted_content"] },
                "headers": { "x-tier": "high" },
            },
        ])
    );
    let gpt = harness.services.model.get(&remote, "gpt").await.expect("read models").expect("the gpt model");
    assert_eq!(gpt.package.as_deref(), Some("@opencode/ai/providers/openai"));
    assert_eq!(encoded(&gpt.variants), encoded(&reasoner.variants));
    let claude =
        harness.services.model.get(&remote, "claude-sonnet-4-5").await.expect("read models").expect("the claude model");
    assert_eq!(claude.package.as_deref(), Some("@opencode/ai/providers/anthropic"));
    assert_eq!(
        encoded(&claude.variants),
        json!([
            { "id": "high", "settings": { "thinking": { "type": "enabled", "budgetTokens": 16000 } } },
            { "id": "max", "settings": { "thinking": { "type": "enabled", "budgetTokens": 31999 } } },
        ])
    );
    let gemini =
        harness.services.model.get(&remote, "gemini-3.1-pro").await.expect("read models").expect("the gemini model");
    assert_eq!(gemini.package.as_deref(), Some("@opencode/ai/providers/google"));
    assert_eq!(
        encoded(&gemini.variants),
        json!([
            { "id": "low", "settings": { "thinkingConfig": { "includeThoughts": true, "thinkingLevel": "low" } } },
            { "id": "high", "settings": { "thinkingConfig": { "includeThoughts": true, "thinkingLevel": "high" } } },
        ])
    );
    let magistral = harness
        .services
        .model
        .get(&remote, "magistral-medium")
        .await
        .expect("read models")
        .expect("the magistral model");
    assert_eq!(magistral.package.as_deref(), Some("@opencode/ai/providers/mistral"));
    assert_eq!(
        encoded(&magistral.variants),
        json!([
            { "id": "low", "settings": { "reasoningEffort": "low" } },
            { "id": "high", "settings": { "reasoningEffort": "high" } },
        ])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loads_native_v2_providers_and_models_from_the_connected_opencode_server() {
    let routes = Routes::new();
    routes.route_json(
        "GET",
        &config_url(ORIGIN),
        json!({
            "providers": {
                "remote": {
                    "canonical": "openai",
                    "name": "Remote",
                    "package": "aisdk:@ai-sdk/openai-compatible",
                    "env": ["REMOTE_API_KEY"],
                    "settings": {
                        "baseURL": format!("{ORIGIN}/v1"),
                        "apiKey": "{env:REMOTE_API_KEY}",
                        "authToken": "provider-auth",
                        "accessToken": "provider-access",
                        "custom": "value",
                    },
                    "headers": { "x-org-id": "org" },
                    "models": {
                        "model": {
                            "modelID": "api-model",
                            "name": "Remote Model",
                            "family": "remote",
                            "capabilities": { "tools": true, "input": ["text", "image"], "output": ["text"] },
                            "settings": {
                                "apiKey": "model-secret",
                                "authToken": "model-auth",
                                "accessToken": "model-access",
                                "temperature": 0.5,
                            },
                            "variants": [{
                                "id": "high",
                                "settings": {
                                    "apiKey": "variant-secret",
                                    "authToken": "variant-auth",
                                    "accessToken": "variant-access",
                                    "temperature": 0.2,
                                },
                                "headers": { "x-variant": "high" },
                            }],
                            "cost": { "input": 1, "output": 2, "cache": { "read": 0.1 } },
                            "limit": { "context": 1000, "output": 100 },
                        },
                        "override": {
                            "name": "Override",
                            "package": "aisdk:@ai-sdk/anthropic",
                            "settings": { "baseURL": format!("{ORIGIN}/anthropic") },
                        },
                        "disabled": { "name": "Disabled", "disabled": true },
                    },
                },
            },
        }),
    );
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = harness
        .services
        .provider
        .transform(|editor| {
            let openai = ProviderID("openai".to_owned());
            editor.update(&openai, |provider| {
                provider.package = "@opencode/ai/providers/openai".to_owned();
                provider.integration_id = Some(IntegrationID("openai".to_owned()));
            });
            editor.models_update(&openai, "api-model", |model| {
                model.package = Some("@opencode/ai/providers/openai".to_owned());
                model.settings = Some(
                    opencode_contract::codec::decode_json::<ModelSettings>(
                        &json!({ "baseURL": "https://upstream.example/v1" }).to_string(),
                    )
                    .expect("the model settings decode"),
                );
                model.variants = vec![ModelVariant {
                    id: "custom".to_owned(),
                    settings: None,
                    headers: Some(JsonMap(vec![("x-custom".to_owned(), "true".to_owned())])),
                    body: Some(JsonMap(vec![("custom".to_owned(), json!(true))])),
                }];
            });
            editor.models_update(&ProviderID("remote".to_owned()), "stale", |_| {});
            Ok(())
        })
        .await;
    harness.credential("opencode", console_key(ORIGIN, None)).await;
    let _context = add_plugin(&mut harness, routes.clone()).await;

    assert_eq!(routes.count("GET", &config_url(ORIGIN)), 1);
    assert_eq!(
        header(&request(&routes, "GET", &config_url(ORIGIN)), "authorization").as_deref(),
        Some("Bearer secret")
    );

    let remote = ProviderID("remote".to_owned());
    let provider = harness.services.provider.get(&remote).await.expect("read providers").expect("the remote provider");
    assert_eq!(provider.canonical, Some(ProviderID("openai".to_owned())));
    assert_eq!(provider.name, "Remote");
    assert_eq!(provider.integration_id, Some(IntegrationID("opencode".to_owned())));
    assert_eq!(provider.package, "@opencode/ai/providers/openai-compatible");
    assert_eq!(
        settings_json(&provider),
        Some(json!({ "baseURL": format!("{ORIGIN}/v1"), "custom": "value", "provider": "openai" }))
    );
    assert_eq!(headers(&provider), Some([("x-org-id".to_owned(), "org".to_owned())].into_iter().collect()));
    assert!(
        harness
            .services
            .integration
            .get(&IntegrationID("remote".to_owned()))
            .await
            .expect("read integrations")
            .is_none()
    );

    let model = harness.services.model.get(&remote, "model").await.expect("read models").expect("the remote model");
    assert_eq!(model.model_id, "api-model");
    assert_eq!(model.canonical, Some(ProviderID("openai".to_owned())));
    assert_eq!(model.name, "Remote Model");
    assert_eq!(model.family.as_deref(), Some("remote"));
    assert!(model.capabilities.tools);
    assert_eq!(model.capabilities.input, ["text", "image"]);
    assert_eq!(model.capabilities.output, ["text"]);
    assert_eq!(encoded(&model.cost), json!([{ "input": 1, "output": 2, "cache": { "read": 0.1, "write": 0 } }]));
    assert_eq!(model.limit.context, 1000.0);
    assert_eq!(model.limit.output, 100.0);
    assert_eq!(model.package.as_deref(), Some("@opencode/ai/providers/openai-compatible"));
    assert_eq!(
        encoded(model.settings.as_ref().expect("the model settings")),
        json!({ "baseURL": format!("{ORIGIN}/v1"), "custom": "value", "temperature": 0.5, "provider": "openai" })
    );
    assert_eq!(model.headers.as_ref().map(encoded), Some(json!({ "x-org-id": "org" })));
    assert_eq!(
        encoded(&model.variants),
        json!([
            {
                "id": "custom",
                "headers": { "x-custom": "true" },
                "body": { "custom": true },
            },
            {
                "id": "high",
                "settings": { "temperature": 0.2 },
                "headers": { "x-variant": "high" },
            },
        ])
    );

    let override_model =
        harness.services.model.get(&remote, "override").await.expect("read models").expect("the override model");
    assert_eq!(override_model.package.as_deref(), Some("@opencode/ai/providers/anthropic"));
    assert_eq!(
        encoded(override_model.settings.as_ref().expect("the override settings")).get("baseURL"),
        Some(&json!(format!("{ORIGIN}/anthropic")))
    );
    let disabled = harness.services.model.get(&remote, "disabled").await.expect("read models").expect("the disabled");
    assert!(!disabled.enabled);
    assert!(harness.services.model.get(&remote, "stale").await.expect("read models").is_some());
    let snapshot = harness.services.provider.snapshot().await.expect("read the provider snapshot");
    let openai = snapshot.records.get("openai").expect("the openai provider");
    assert_eq!(
        openai.models.get("api-model").and_then(|model| model.settings.as_ref()).map(encoded),
        Some(json!({ "baseURL": "https://upstream.example/v1" }))
    );
    assert!(
        harness.services.model.get(&ProviderID("openai".to_owned()), "api-model").await.expect("read models").is_none()
    );

    let replacement = harness
        .services
        .credentials
        .create(CreateInput {
            id: None,
            integration_id: integration_id(),
            value: CredentialValue::Key(CredentialKey {
                r#type: CredentialKeyType::Key,
                key: "replacement".to_owned(),
                metadata: Some(JsonMap(vec![("server".to_owned(), json!(ORIGIN))])),
            }),
            label: None,
            activate: None,
        })
        .expect("create the replacement credential");
    harness.services.integration.connection().activate(&replacement.id).await.expect("activate the replacement");
    for _ in 0..2000 {
        if routes.count("GET", &config_url(ORIGIN)) >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let requests = routes.requests();
    let loads: Vec<&opencode_ai::route::executor::PreparedHttp> =
        requests.iter().filter(|request| request.url == config_url(ORIGIN)).collect();
    assert_eq!(loads.len(), 2);
    assert_eq!(header(loads[1], "authorization").as_deref(), Some("Bearer replacement"));
    assert_eq!(harness.services.credentials.list(&integration_id()).expect("list the OpenCode credentials").len(), 2);
}

#[tokio::test(start_paused = true)]
async fn activates_when_the_console_accepts_the_config_request_but_never_answers() {
    let routes = Routes::new();
    let url = config_url("https://console.example");
    routes.hold("GET", &url);
    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential("opencode", console_key("https://console.example", None)).await;
    let context = plugin_context_with(&harness, routes.clone());
    let mut builtin = Builtin::default();
    let mut registration = Box::pin(opencode::register(&harness.services, &context, &mut builtin));
    let mut completed = false;
    tokio::select! {
        _ = routes.arrived() => {}
        _ = &mut registration => {
            completed = true;
        }
    }
    for _ in 0..29 {
        if completed {
            break;
        }
        tokio::select! {
            _ = &mut registration => {
                completed = true;
                break;
            }
            _ = tokio::time::advance(Duration::from_secs(1)) => {}
        }
    }
    if !completed {
        registration.as_mut().await;
    }
    drop(registration);
    harness.record(builtin);

    assert_eq!(routes.count("GET", &url), 4);
    let integration = harness
        .services
        .integration
        .get(&integration_id())
        .await
        .expect("get the OpenCode integration")
        .expect("the OpenCode integration");
    assert_eq!(integration.connections.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_a_rejected_console_refresh_token_as_signed_out_without_removing_the_credential() {
    let routes = Routes::new();
    routes.route_with("POST", &format!("{ORIGIN}/auth/device/token"), |_| {
        Reply::json_status(json!({ "error": "invalid_grant", "error_description": "refresh token revoked" }), 400)
    });
    let mut harness = start(Input::new(Vec::new())).await;
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
    add_plugin(&mut harness, routes).await;

    assert_eq!(
        integration_status(&harness).await,
        Some(ConnectionStatus {
            status: opencode_contract::generated::ConnectionStatusStatus::NeedsAuth,
            message: "Reconnect OpenCode Console to continue".to_owned(),
            url: None,
        })
    );
    let stored = harness.services.credentials.get(&stored.id).expect("get the credential").expect("the credential");
    assert_eq!(stored.value, oauth(ORIGIN));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_a_console_session_rejected_before_its_token_expires_as_signed_out() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let revoked = Arc::new(AtomicBool::new(true));
    let routes = Routes::new();
    routes.route_with("GET", &config_url(ORIGIN), {
        let revoked = revoked.clone();
        move |_| {
            if revoked.load(Ordering::SeqCst) {
                return Reply::json_status(
                    json!({ "_tag": "Unauthorized", "message": "Invalid or expired session" }),
                    401,
                );
            }
            Reply::json(json!({ "providers": {} }))
        }
    });
    let mut harness = start(Input::new(Vec::new())).await;
    let stored = harness
        .services
        .credentials
        .create(CreateInput {
            id: None,
            integration_id: integration_id(),
            value: CredentialValue::Oauth(CredentialOAuth {
                r#type: CredentialOAuthType::Oauth,
                method_id: method_id(),
                access: "revoked".to_owned(),
                refresh: "refresh".to_owned(),
                expires: 9_007_199_254_740_991.0,
                metadata: Some(JsonMap(vec![
                    ("server".to_owned(), json!(ORIGIN)),
                    ("orgID".to_owned(), json!("org_acme")),
                    ("orgName".to_owned(), json!("Acme")),
                ])),
            }),
            label: None,
            activate: None,
        })
        .expect("create the OpenCode credential");
    let context = add_plugin(&mut harness, routes.clone()).await;

    assert_eq!(
        integration_status(&harness).await,
        Some(ConnectionStatus {
            status: opencode_contract::generated::ConnectionStatusStatus::NeedsAuth,
            message: "Reconnect OpenCode Console to continue".to_owned(),
            url: None,
        })
    );
    let current = harness.services.credentials.get(&stored.id).expect("get the credential").expect("the credential");
    let CredentialValue::Oauth(value) = &current.value else { panic!("the credential is OAuth") };
    assert_eq!(value.access, "revoked");

    revoked.store(false, Ordering::SeqCst);
    opencode::refresh(&harness.services, &context, Duration::ZERO).await;
    assert!(integration_status(&harness).await.is_none());
    assert_eq!(routes.count("POST", &format!("{ORIGIN}/auth/device/token")), 0);
}

fn search_results() -> Value {
    json!([{
        "url": "https://github.com/anomalyco/opencode",
        "title": "OpenCode",
        "content": "Open source AI coding agent.",
        "time": { "published": 1_700_000_000_000i64 },
    }])
}

fn console_web_search_reply(state: &Mutex<Value>) -> Reply {
    let state = state.lock().expect("the hosted search state lock is never poisoned");
    Reply::json(json!({ "providerID": state["providerID"], "results": search_results() }))
}

fn advertised_web_search(advertised: bool) -> Value {
    if advertised {
        json!({ "providers": {}, "websearch": { "provider": "opencode" } })
    } else {
        json!({ "providers": {} })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loads_and_executes_hosted_web_search_from_the_connected_opencode_server() {
    let state = Arc::new(Mutex::new(json!({ "advertised": true, "providerID": "opencode" })));
    let routes = Routes::new();
    routes.route_with("GET", &config_url(ORIGIN), {
        let state = state.clone();
        move |_| {
            let state = state.lock().expect("the hosted search state lock is never poisoned");
            Reply::json(advertised_web_search(state["advertised"].as_bool().unwrap_or(false)))
        }
    });
    for url in [format!("{ORIGIN}/api/websearch"), format!("{ORIGIN}/other/api/websearch")] {
        routes.route_with("POST", &url, {
            let state = state.clone();
            move |_| console_web_search_reply(&state)
        });
    }
    let mut harness = start(Input::new(Vec::new())).await;
    let stored = harness
        .services
        .credentials
        .create(CreateInput {
            id: None,
            integration_id: integration_id(),
            value: console_key(ORIGIN, Some(("org_test", "Test"))),
            label: None,
            activate: None,
        })
        .expect("create the OpenCode credential");
    let context = add_plugin(&mut harness, routes.clone()).await;

    let search = context.console.web_search().expect("the Console offers hosted web search");
    assert_eq!(search.provider_id.0, "opencode");
    assert_eq!(search.name, "OpenCode Web Search");
    assert_eq!(json!(search.execute("effect web search").await.expect("the hosted search")), search_results());

    let sent = routes.requests();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].method, "GET");
    assert_eq!(sent[0].url, config_url(ORIGIN));
    assert_eq!(header(&sent[0], "authorization").as_deref(), Some("Bearer secret"));
    assert_eq!(header(&sent[0], "x-org-id").as_deref(), Some("org_test"));
    assert_eq!(sent[1].method, "POST");
    assert_eq!(sent[1].url, format!("{ORIGIN}/api/websearch"));
    assert_eq!(header(&sent[1], "authorization").as_deref(), Some("Bearer secret"));
    assert_eq!(header(&sent[1], "x-org-id").as_deref(), Some("org_test"));
    assert_eq!(json_body(&sent[1]), json!({ "query": "effect web search", "providerID": "opencode" }));

    harness
        .services
        .credentials
        .update(
            &stored.id,
            Update { label: None, value: Some(console_key_with(ORIGIN, "replacement", Some(("org_test", "Test")))) },
        )
        .expect("update the OpenCode credential");
    search.execute("fresh credential").await.expect("the hosted search with a fresh credential");
    let last = routes.requests().last().cloned().expect("the fresh credential request");
    assert_eq!(header(&last, "authorization").as_deref(), Some("Bearer replacement"));
    assert_eq!(json_body(&last), json!({ "query": "fresh credential", "providerID": "opencode" }));

    harness
        .services
        .credentials
        .update(
            &stored.id,
            Update {
                label: None,
                value: Some(console_key_with(
                    &format!("{ORIGIN}/other///?ignored=true#ignored"),
                    "moved",
                    Some(("org_test", "Test")),
                )),
            },
        )
        .expect("move the OpenCode server");
    search.execute("updated server").await.expect("the hosted search on the updated server");
    let last = routes.requests().last().cloned().expect("the updated server request");
    assert_eq!(last.method, "POST");
    assert_eq!(last.url, format!("{ORIGIN}/other/api/websearch"));
    assert_eq!(header(&last, "authorization").as_deref(), Some("Bearer moved"));
    assert_eq!(header(&last, "x-org-id").as_deref(), Some("org_test"));

    state.lock().expect("lock the hosted search state")["providerID"] = json!("unexpected");
    assert_eq!(
        search.execute("wrong provider").await.expect_err("the provider mismatch fails"),
        "OpenCode web search returned provider unexpected instead of opencode"
    );

    state.lock().expect("lock the hosted search state")["advertised"] = json!(false);
    routes.hold("GET", &config_url(ORIGIN));
    harness
        .services
        .credentials
        .create(CreateInput {
            id: None,
            integration_id: integration_id(),
            value: console_key_with(ORIGIN, "switched", Some(("org_switched", "Switched"))),
            label: None,
            activate: None,
        })
        .expect("switch the OpenCode account");
    routes.arrived().await;
    let searches = routes.count("POST", &format!("{ORIGIN}/api/websearch"));
    assert_eq!(
        search.execute("switch race").await.expect_err("the switched connection fails"),
        "OpenCode Console connection changed"
    );
    assert_eq!(routes.count("POST", &format!("{ORIGIN}/api/websearch")), searches);
    routes.release();
    for _ in 0..2000 {
        if context.console.web_search().is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert!(context.console.web_search().is_none());
    let last = routes.requests().last().cloned().expect("the switched account request");
    assert_eq!(last.url, config_url(ORIGIN));
    assert_eq!(header(&last, "authorization").as_deref(), Some("Bearer switched"));
    assert_eq!(header(&last, "x-org-id").as_deref(), Some("org_switched"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sends_hosted_search_to_the_default_console_when_the_credential_names_none() {
    let console = opencode_catalog::console_access::DEFAULT_SERVER;
    let routes = Routes::new();
    routes.route_json("GET", &config_url(console), advertised_web_search(true));
    routes.route_json("POST", &format!("{console}/api/websearch"), json!({ "providerID": "opencode", "results": [] }));
    let mut harness = start(Input::new(Vec::new())).await;
    harness
        .credential(
            "opencode",
            CredentialValue::Key(CredentialKey {
                r#type: CredentialKeyType::Key,
                key: "secret".to_owned(),
                metadata: None,
            }),
        )
        .await;
    let context = add_plugin(&mut harness, routes.clone()).await;

    let search = context.console.web_search().expect("the Console offers hosted web search");
    assert_eq!(search.execute("default Console").await.expect("the hosted search"), Vec::<Value>::new());
    assert_eq!(
        routes.requests().into_iter().map(|request| request.url).collect::<Vec<_>>(),
        [config_url(console), format!("{console}/api/websearch")]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn follows_a_hosted_search_redirect_without_sending_credentials_to_another_origin() {
    let server = format!("{ORIGIN}/console");
    let routes = Routes::new();
    routes.route_json("GET", &config_url(&server), advertised_web_search(true));
    routes.route_redirect("POST", &format!("{server}/api/websearch"), 307, &format!("{ORIGIN}/outside-console"));
    for url in [format!("{ORIGIN}/outside-console"), "http://127.0.0.1:9/outside-console".to_owned()] {
        routes.route_json("POST", &url, json!({ "providerID": "opencode", "results": [] }));
    }
    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential("opencode", console_key(&server, Some(("org_test", "Test")))).await;
    let context = add_plugin(&mut harness, routes.clone()).await;

    let search = context.console.web_search().expect("the Console offers hosted web search");
    assert_eq!(search.execute("private search").await.expect("the same-origin search"), Vec::<Value>::new());
    let last = routes.requests().last().cloned().expect("the same-origin redirect request");
    assert_eq!(last.url, format!("{ORIGIN}/outside-console"));
    assert_eq!(last.method, "POST");
    assert_eq!(header(&last, "authorization").as_deref(), Some("Bearer secret"));
    assert_eq!(json_body(&last), json!({ "query": "private search", "providerID": "opencode" }));

    routes.set_redirect("POST", &format!("{server}/api/websearch"), 307, "http://127.0.0.1:9/outside-console");
    assert_eq!(search.execute("private search").await.expect("the cross-origin search"), Vec::<Value>::new());
    let last = routes.requests().last().cloned().expect("the cross-origin redirect request");
    assert_eq!(last.url, "http://127.0.0.1:9/outside-console");
    assert_eq!(last.method, "POST");
    assert_eq!(header(&last, "authorization"), None);
    assert_eq!(header(&last, "x-org-id"), None);
    assert_eq!(json_body(&last), json!({ "query": "private search", "providerID": "opencode" }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn follows_a_302_hosted_search_redirect_as_a_get() {
    let routes = Routes::new();
    routes.route_json("GET", &config_url(ORIGIN), advertised_web_search(true));
    routes.route_redirect("POST", &format!("{ORIGIN}/api/websearch"), 302, &format!("{ORIGIN}/gateway/search"));
    routes.route_json("GET", &format!("{ORIGIN}/gateway/search"), json!({ "providerID": "opencode", "results": [] }));
    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential("opencode", console_key(ORIGIN, Some(("org_test", "Test")))).await;
    let context = add_plugin(&mut harness, routes.clone()).await;

    let search = context.console.web_search().expect("the Console offers hosted web search");
    assert_eq!(search.execute("gateway search").await.expect("the redirected search"), Vec::<Value>::new());
    let last = routes.requests().last().cloned().expect("the gateway request");
    assert_eq!(last.url, format!("{ORIGIN}/gateway/search"));
    assert_eq!(last.method, "GET");
    assert_eq!(header(&last, "authorization").as_deref(), Some("Bearer secret"));
    assert!(last.body.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stops_following_a_hosted_search_redirect_loop_at_the_hop_limit() {
    let routes = Routes::new();
    routes.route_json("GET", &config_url(ORIGIN), advertised_web_search(true));
    routes.route_redirect("POST", &format!("{ORIGIN}/api/websearch"), 302, &format!("{ORIGIN}/loop"));
    routes.route_redirect("GET", &format!("{ORIGIN}/loop"), 302, &format!("{ORIGIN}/loop"));
    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential("opencode", console_key(ORIGIN, Some(("org_test", "Test")))).await;
    let context = add_plugin(&mut harness, routes.clone()).await;

    let search = context.console.web_search().expect("the Console offers hosted web search");
    assert_eq!(
        search.execute("looping search").await.expect_err("the redirect loop fails"),
        "Too many redirects (more than 5)"
    );
    assert_eq!(routes.count("GET", &format!("{ORIGIN}/loop")), 5);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closes_a_rejected_hosted_search_response_without_waiting_for_its_body() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let routes = Routes::new();
    routes.route_json("GET", &config_url(ORIGIN), advertised_web_search(true));
    routes.route_hanging("POST", &format!("{ORIGIN}/api/websearch"), 503, cancelled.clone());
    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential("opencode", console_key(ORIGIN, Some(("org_test", "Test")))).await;
    let context = add_plugin(&mut harness, routes.clone()).await;

    let search = context.console.web_search().expect("the Console offers hosted web search");
    let error = tokio::time::timeout(Duration::from_secs(5), search.execute("rejected search"))
        .await
        .expect("the rejected search returns without reading its body")
        .expect_err("the rejected search fails");
    assert_eq!(error, "HTTP status 503");
    assert!(cancelled.load(Ordering::SeqCst));
}

#[tokio::test(start_paused = true)]
async fn refreshes_hosted_search_with_console_config_retains_it_on_failure_and_skips_unchanged_snapshots() {
    let state = Arc::new(Mutex::new(json!({ "advertised": false, "failing": false, "requests": 0 })));
    let routes = Routes::new();
    routes.route_with("GET", &config_url(ORIGIN), {
        let state = state.clone();
        move |_| {
            let mut state = state.lock().expect("the hosted search state lock is never poisoned");
            state["requests"] = json!(state["requests"].as_u64().unwrap_or(0) + 1);
            if state["failing"].as_bool().unwrap_or(false) {
                return Reply::status(502);
            }
            Reply::json(advertised_web_search(state["advertised"].as_bool().unwrap_or(false)))
        }
    });
    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential("opencode", console_key(ORIGIN, None)).await;
    let context = add_plugin(&mut harness, routes.clone()).await;
    let requests =
        || state.lock().expect("the hosted search state lock is never poisoned")["requests"].as_u64().unwrap_or(0);

    assert_eq!(requests(), 1);
    assert!(context.console.web_search().is_none());

    state.lock().expect("lock the hosted search state")["advertised"] = json!(true);
    advance_for(Duration::from_secs(50)).await;
    assert_eq!(requests(), 1);
    assert!(context.console.web_search().is_none());

    advance_for(Duration::from_secs(10)).await;
    advance_until(|| context.console.web_search().is_some()).await;
    assert_eq!(requests(), 2);
    assert!(context.console.web_search().is_some());

    advance_for(Duration::from_secs(60)).await;
    advance_until(|| requests() == 3).await;
    assert!(context.console.web_search().is_some());

    state.lock().expect("lock the hosted search state")["failing"] = json!(true);
    advance_for(Duration::from_secs(60)).await;
    advance_until(|| requests() == 7).await;
    assert!(context.console.web_search().is_some());

    {
        let mut state = state.lock().expect("lock the hosted search state");
        state["failing"] = json!(false);
        state["advertised"] = json!(false);
    }
    advance_for(Duration::from_secs(60)).await;
    advance_until(|| context.console.web_search().is_none()).await;
    assert_eq!(requests(), 8);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offers_console_web_search_only_when_the_console_selects_it_and_user_config_still_wins() {
    let selected = Arc::new(AtomicBool::new(false));
    let routes = Routes::new();
    routes.route_with("GET", &config_url(ORIGIN), {
        let selected = selected.clone();
        move |_| Reply::json(advertised_web_search(selected.load(Ordering::SeqCst)))
    });
    routes.route_json("POST", &format!("{ORIGIN}/api/websearch"), json!({ "providerID": "opencode", "results": [] }));
    let user = json!({ "websearch": { "provider": "mine" } });
    let mut harness = start(Input::new(vec![user])).await;
    harness.credential("opencode", console_key(ORIGIN, None)).await;
    let context = add_plugin(&mut harness, routes.clone()).await;

    assert!(context.console.web_search().is_none());

    selected.store(true, Ordering::SeqCst);
    opencode::refresh(&harness.services, &context, Duration::ZERO).await;
    let search = context.console.web_search().expect("the Console offers hosted web search");
    assert_eq!(search.provider_id.0, "opencode");
    assert_eq!(search.name, "OpenCode Web Search");

    let selected_providers: Vec<String> = harness
        .services
        .config
        .entries()
        .into_iter()
        .filter_map(|entry| match entry {
            Entry::Document(document) => document_json(&document)
                .get("websearch")
                .and_then(|websearch| websearch.get("provider"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            Entry::Directory { .. } => None,
        })
        .collect();
    assert_eq!(selected_providers, ["opencode".to_owned(), "mine".to_owned()]);
}

const LISTS_HEADER: &str = "x-opencode-instructions-skills";

fn lists_headers() -> Vec<(String, String)> {
    vec![(LISTS_HEADER.to_owned(), "1".to_owned())]
}

fn list_url(name: &str) -> String {
    format!("{ORIGIN}/api/v2/{name}")
}

fn skill_url(slug: &str) -> String {
    format!("{ORIGIN}/api/v2/skills/{slug}")
}

fn skill_bundle() -> Value {
    json!({
        "slug": "deploy",
        "files": [{
            "path": "SKILL.md",
            "content": "---\nname: Deploy\ndescription: Deploys the service.\n---\n\n# Deploy\n",
        }],
    })
}

fn instructions_of(console: &opencode::ConsoleHandle) -> Vec<String> {
    console.instructions().into_iter().map(|instruction| instruction.content).collect()
}

async fn eventually_console_skills(console: &opencode::ConsoleHandle) -> Vec<opencode::ConsoleSkill> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let skills = console.skills();
            if !skills.is_empty() {
                return skills;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("the Console skills did not load")
}

async fn eventually_console_requests(routes: &Routes) -> Vec<PreparedHttp> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let requests = routes.requests();
            if requests.len() >= 4 {
                return requests;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "the Console config and its lists were not requested: {:?}",
            routes.requests().iter().map(|request| request.url.clone()).collect::<Vec<_>>()
        )
    })
}

fn spawn_plugin(
    harness: &Harness,
    http: Arc<dyn opencode_ai::HttpSender>,
) -> (PluginContext, tokio::task::JoinHandle<Builtin>) {
    let context = plugin_context_with(harness, http);
    let task = {
        let services = harness.services.clone();
        let context = context.clone();
        tokio::spawn(async move {
            let mut builtin = Builtin::default();
            opencode::register(&services, &context, &mut builtin).await;
            builtin
        })
    };
    (context, task)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn downloads_a_skill_through_a_redirect_without_sending_credentials_to_another_origin() {
    let archive = "https://storage.example.com/archive/deploy.json?expires=1&sig=a%20b~c";
    let routes = Routes::new();
    routes.route_json_with_headers("GET", &config_url(ORIGIN), lists_headers(), json!({ "providers": {} }));
    routes.route_json("GET", &list_url("instructions"), json!({ "instructions": [] }));
    routes.route_json("GET", &list_url("skills"), json!({ "skills": [{ "slug": "deploy", "version": "1" }] }));
    routes.route_redirect("GET", &skill_url("deploy"), 302, archive);
    routes.route_json("GET", archive, skill_bundle());

    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential("opencode", console_key(ORIGIN, Some(("org_test", "Test")))).await;
    let context = add_plugin(&mut harness, routes.clone()).await;

    let skills = eventually_console_skills(&context.console).await;
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].slug, "deploy");
    assert!(skills[0].main.contains("Deploys the service."));
    assert!(skills[0].files.is_empty());

    let download = request(&routes, "GET", &skill_url("deploy"));
    assert_eq!(header(&download, "authorization").as_deref(), Some("Bearer secret"));
    assert_eq!(header(&download, "x-org-id").as_deref(), Some("org_test"));

    let archive_request = request(&routes, "GET", archive);
    assert_eq!(header(&archive_request, "authorization"), None);
    assert_eq!(header(&archive_request, "x-org-id"), None);
}

#[tokio::test(start_paused = true)]
async fn times_out_a_skill_download_whose_body_never_completes() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let routes = Routes::new();
    routes.route_json_with_headers("GET", &config_url(ORIGIN), lists_headers(), json!({ "providers": {} }));
    routes.route_json("GET", &list_url("instructions"), json!({ "instructions": [] }));
    routes.route_json("GET", &list_url("skills"), json!({ "skills": [{ "slug": "deploy", "version": "1" }] }));
    routes.route_hanging("GET", &skill_url("deploy"), 200, cancelled.clone());

    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential("opencode", console_key(ORIGIN, Some(("org_test", "Test")))).await;
    let context = add_plugin(&mut harness, routes.clone()).await;

    advance_until(|| cancelled.load(Ordering::SeqCst)).await;

    assert!(context.console.skills().is_empty());
    assert_eq!(routes.count("GET", &skill_url("deploy")), 1);
}

#[tokio::test(start_paused = true)]
async fn keeps_console_instructions_and_skills_through_an_edit_or_a_console_error_and_clears_them_on_404() {
    let instruction = Arc::new(Mutex::new("Use tabs.".to_owned()));
    let status = Arc::new(Mutex::new(200u16));
    let routes = Routes::new();
    routes.route_json_with_headers("GET", &config_url(ORIGIN), lists_headers(), json!({ "providers": {} }));
    routes.route_with("GET", &list_url("instructions"), {
        let instruction = instruction.clone();
        let status = status.clone();
        move |_| {
            let status = *status.lock().expect("lock the Console list status");
            if status != 200 {
                return Reply::status(status);
            }
            Reply::json(json!({
                "instructions": [{ "id": "style", "content": instruction.lock().expect("lock the instruction").clone() }],
            }))
        }
    });
    routes.route_with("GET", &list_url("skills"), {
        let status = status.clone();
        move |_| {
            let status = *status.lock().expect("lock the Console list status");
            if status != 200 {
                return Reply::status(status);
            }
            Reply::json(json!({ "skills": [{ "slug": "deploy", "version": "1" }] }))
        }
    });
    routes.route_json("GET", &skill_url("deploy"), skill_bundle());

    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential("opencode", console_key(ORIGIN, None)).await;
    let context = add_plugin(&mut harness, routes.clone()).await;

    advance_until(|| context.console.skills().len() == 1).await;
    assert_eq!(instructions_of(&context.console), ["Use tabs.".to_owned()]);

    *instruction.lock().expect("lock the instruction") = "Use spaces.".to_owned();
    advance_until(|| instructions_of(&context.console) == ["Use spaces.".to_owned()]).await;
    assert_eq!(context.console.skills().len(), 1);

    let before = routes.count("GET", &list_url("instructions"));
    *status.lock().expect("lock the Console list status") = 503;
    advance_until(|| routes.count("GET", &list_url("instructions")) > before).await;
    advance_for(Duration::from_secs(10)).await;
    assert_eq!(instructions_of(&context.console), ["Use spaces.".to_owned()]);
    assert_eq!(context.console.skills().len(), 1);

    *status.lock().expect("lock the Console list status") = 404;
    advance_until(|| context.console.instructions().is_empty()).await;
    assert!(context.console.skills().is_empty());
}

#[tokio::test(start_paused = true)]
async fn applies_console_policies_while_its_instructions_and_skills_fail_and_keeps_a_list_over_a_malformed_one() {
    let sudo = json!({ "action": "tool.use", "resource": "shell:sudo *", "effect": "deny" });
    let state = Arc::new(Mutex::new("down".to_owned()));
    let routes = Routes::new();
    routes.route_json_with_headers(
        "GET",
        &config_url(ORIGIN),
        lists_headers(),
        json!({ "providers": {}, "policies": [sudo.clone()] }),
    );
    routes.route_with("GET", &list_url("instructions"), {
        let state = state.clone();
        move |_| match state.lock().expect("lock the Console list state").as_str() {
            "down" => Reply::status(503),
            "malformed" => Reply::json(json!({ "instructions": [{ "id": "style" }] })),
            _ => Reply::json(json!({ "instructions": [{ "id": "style", "content": "Use tabs." }] })),
        }
    });
    routes.route_with("GET", &list_url("skills"), {
        let state = state.clone();
        move |_| match state.lock().expect("lock the Console list state").as_str() {
            "down" => Reply::status(503),
            _ => Reply::json(json!({ "skills": [] })),
        }
    });

    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential("opencode", console_key(ORIGIN, None)).await;
    let (context, plugin) = spawn_plugin(&harness, routes.clone());
    advance_for(Duration::from_secs(10)).await;
    let builtin = plugin.await.expect("the OpenCode plugin registers");
    harness.record(builtin);

    let document = console_document(&harness).expect("the Console document is integrated");
    assert_eq!(policy_list(&document), json!([sudo.clone()]));
    assert!(context.console.instructions().is_empty());

    *state.lock().expect("lock the Console list state") = "ok".to_owned();
    advance_until(|| instructions_of(&context.console) == ["Use tabs.".to_owned()]).await;

    let before = routes.count("GET", &list_url("instructions"));
    *state.lock().expect("lock the Console list state") = "malformed".to_owned();
    advance_until(|| routes.count("GET", &list_url("instructions")) > before).await;
    advance_tick().await;
    assert_eq!(instructions_of(&context.console), ["Use tabs.".to_owned()]);
    let document = console_document(&harness).expect("the Console document is integrated");
    assert_eq!(policy_list(&document), json!([sudo]));
}

#[tokio::test(start_paused = true)]
async fn requests_console_instructions_and_skills_together() {
    let routes = Routes::new();
    routes.route_json_with_headers("GET", &config_url(ORIGIN), lists_headers(), json!({ "providers": {} }));
    routes.route_json(
        "GET",
        &list_url("instructions"),
        json!({ "instructions": [{ "id": "style", "content": "Use tabs." }] }),
    );
    routes.route_json("GET", &list_url("skills"), json!({ "skills": [] }));
    routes.hold("GET", &list_url("instructions"));

    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential("opencode", console_key(ORIGIN, None)).await;
    let (context, plugin) = spawn_plugin(&harness, routes.clone());
    routes.arrived().await;
    assert_eq!(routes.count("GET", &list_url("skills")), 1);
    let mut requests: Vec<String> = routes.requests().into_iter().map(|request| request.url).collect();
    requests.sort();
    assert_eq!(requests, [config_url(ORIGIN), list_url("instructions"), list_url("skills")]);

    routes.release();
    let builtin = plugin.await.expect("the OpenCode plugin registers");
    harness.record(builtin);
    assert_eq!(instructions_of(&context.console), ["Use tabs.".to_owned()]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keeps_credentials_on_a_same_origin_skill_redirect() {
    let asset = format!("{ORIGIN}/assets/deploy.json");
    let routes = Routes::new();
    routes.route_json_with_headers("GET", &config_url(ORIGIN), lists_headers(), json!({ "providers": {} }));
    routes.route_json("GET", &list_url("instructions"), json!({ "instructions": [] }));
    routes.route_json("GET", &list_url("skills"), json!({ "skills": [{ "slug": "deploy", "version": "1" }] }));
    routes.route_redirect("GET", &skill_url("deploy"), 302, &asset);
    routes.route_json("GET", &asset, skill_bundle());

    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential("opencode", console_key(ORIGIN, Some(("org_test", "Test")))).await;
    let context = add_plugin(&mut harness, routes.clone()).await;

    let skills = eventually_console_skills(&context.console).await;
    assert_eq!(skills.len(), 1);

    let download = request(&routes, "GET", &asset);
    assert_eq!(header(&download, "authorization").as_deref(), Some("Bearer secret"));
}

#[tokio::test(start_paused = true)]
async fn does_not_keep_a_skill_download_that_a_later_sync_cleared() {
    let routes = Routes::new();
    routes.route_json_with_headers("GET", &config_url(ORIGIN), lists_headers(), json!({ "providers": {} }));
    routes.route_json("GET", &list_url("instructions"), json!({ "instructions": [] }));
    routes.route_json("GET", &list_url("skills"), json!({ "skills": [{ "slug": "deploy", "version": "1" }] }));
    routes.hold("GET", &skill_url("deploy"));
    routes.route_json("GET", &skill_url("deploy"), skill_bundle());

    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential("opencode", console_key(ORIGIN, None)).await;
    let context = add_plugin(&mut harness, routes.clone()).await;

    routes.arrived().await;
    assert!(context.console.skills().is_empty());

    routes.set_json("GET", &list_url("skills"), json!({ "skills": [] }));
    opencode::refresh(&harness.services, &context, Duration::ZERO).await;

    routes.release();
    advance_for(Duration::from_secs(1)).await;
    assert!(context.console.skills().is_empty());
}

#[tokio::test(start_paused = true)]
async fn releases_the_console_explainers_when_the_builtin_is_dropped() {
    let routes = Routes::new();
    routes.route_json("GET", &config_url(ORIGIN), json!({ "providers": {} }));
    let mut harness = start(Input::new(Vec::new())).await;
    harness.credential("opencode", console_key(ORIGIN, None)).await;
    let context = add_plugin(&mut harness, routes.clone()).await;
    assert_eq!(routes.count("GET", &config_url(ORIGIN)), 1);

    drop(std::mem::take(&mut harness.registrations));
    tokio::time::advance(Duration::from_secs(30)).await;

    let response = context.hooks.trigger_http_response(console_response("opencode", 403, &json!({}))).await;
    assert_eq!(response.response.status, 403);
    assert_eq!(routes.count("GET", &config_url(ORIGIN)), 1);

    for _ in 0..100 {
        if Arc::strong_count(&context.console) == 1 && Arc::strong_count(&context.hooks) == 1 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(Arc::strong_count(&context.console), 1);
    assert_eq!(Arc::strong_count(&context.hooks), 1);
}
