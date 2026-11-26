use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use indexmap::IndexMap;
use opencode_ai::route::executor::PreparedHttp;
use opencode_catalog::builtin::Builtin;
use opencode_catalog::integration::{Implementation, OAuthImplementation, OAuthMode};
use opencode_catalog::provider_plugin::github_copilot::{
    self, ID, Options, UtilityTitleInput, UtilityTitleMessage, UtilityTitleRequest, models, oauth,
};
use opencode_catalog::provider_plugin::hooks::{
    Compaction, HttpRequest, ModelRequest, ModelResolve, ProviderHooks, RequestKind, Title,
};
use opencode_catalog::provider_plugin::{AppInfo, ORDER, PluginContext, SessionRef};
use opencode_contract::codec::{JsonMap, decode_json};
use opencode_contract::generated::{
    AgentID, ConnectionCredentialInfo, ConnectionCredentialInfoMethod, ConnectionCredentialInfoType, ConnectionInfo,
    CredentialID, CredentialOAuth, CredentialOAuthType, CredentialValue, FormField1, FormValue, IntegrationEnvMethod,
    IntegrationEnvMethodType, IntegrationID, IntegrationKeyMethod, IntegrationKeyMethodType, IntegrationMethod,
    IntegrationMethodID, IntegrationOAuthMethodType, ModelInfo, ModelRef, ProviderID, ProviderInfoActivation,
    SessionID,
};
use serde_json::{Value, json};

use crate::builtin::{Harness, Input, start};
use crate::provider_plugin::support::{Exchange, FakeSessions, RecordedSender, Reply, Routes, plugin_context_with};

const PROVIDER_ID: &str = "github-copilot";

const PUBLIC_USER: &str = "https://api.github.com/copilot_internal/user";

const ENTERPRISE_USER: &str = "https://api.company.ghe.com/copilot_internal/user";

const BUSINESS_API: &str = "https://api.business.githubcopilot.com";

const BUSINESS_MODELS: &str = "https://api.business.githubcopilot.com/models";

const INDIVIDUAL_API: &str = "https://api.individual.githubcopilot.com";

const INDIVIDUAL_MODELS: &str = "https://api.individual.githubcopilot.com/models";

const COPILOT_API: &str = "https://api.githubcopilot.com";

const ANTHROPIC: &str = "@opencode/ai/providers/anthropic";

const OPENAI: &str = "@opencode/ai/providers/openai";

const OPENAI_COMPATIBLE: &str = "@opencode/ai/providers/openai-compatible";

const OPENAI_RESPONSES: &str = "@opencode/ai/providers/openai-compatible-responses";

fn provider_id() -> ProviderID {
    ProviderID(PROVIDER_ID.to_owned())
}

fn integration_id() -> IntegrationID {
    IntegrationID(PROVIDER_ID.to_owned())
}

fn sample_models() -> Value {
    json!({
        "data": [{
            "model_picker_enabled": true,
            "id": "gpt-5",
            "name": "GPT-5",
            "version": "gpt-5",
            "capabilities": {
                "family": "gpt-5",
                "limits": { "max_output_tokens": 1, "max_prompt_tokens": 1 },
                "supports": { "tool_calls": true }
            }
        }]
    })
}

fn model(id: &str, model_id: &str, package: Option<&str>, name: &str, provider: &str) -> Value {
    let mut value = json!({
        "id": id,
        "modelID": model_id,
        "providerID": provider,
        "name": name,
        "capabilities": { "tools": true, "input": ["text"], "output": ["text"] },
        "variants": [],
        "time": { "released": 0 },
        "cost": [],
        "status": "active",
        "enabled": true,
        "limit": { "context": 1000, "output": 100 }
    });
    if let Some(package) = package {
        value.as_object_mut().expect("the model template is an object").insert("package".to_owned(), json!(package));
    }
    value
}

fn decode_models(values: Vec<Value>) -> Vec<ModelInfo> {
    values.into_iter().map(|model| decode_json(&model.to_string()).expect("a seeded model decodes")).collect()
}

fn oauth(metadata: Option<Value>) -> CredentialValue {
    oauth_with("token", metadata)
}

fn oauth_with(access: &str, metadata: Option<Value>) -> CredentialValue {
    CredentialValue::Oauth(CredentialOAuth {
        r#type: CredentialOAuthType::Oauth,
        method_id: IntegrationMethodID(oauth::METHOD_ID.to_owned()),
        access: access.to_owned(),
        refresh: access.to_owned(),
        expires: 0.0,
        metadata: metadata.map(|value| match value {
            Value::Object(object) => JsonMap(object.into_iter().collect()),
            _ => panic!("the credential metadata is a JSON object"),
        }),
    })
}

fn session_fixture() -> (FakeSessions, SessionID, SessionID) {
    let parent = SessionID("ses_parent".to_owned());
    let child = SessionID("ses_child".to_owned());
    let sessions = FakeSessions::default();
    sessions.insert(SessionRef { id: parent.clone(), parent_id: None, fork_session_id: None });
    sessions.insert(SessionRef { id: child.clone(), parent_id: Some(parent.clone()), fork_session_id: None });
    (sessions, parent, child)
}

async fn register_hooks(harness: &mut Harness, sessions: FakeSessions, routes: Arc<Routes>) -> Arc<ProviderHooks> {
    let mut context = plugin_context_with(harness, routes);
    context.sessions = Arc::new(sessions);
    let hooks = context.hooks.clone();
    register_copilot(harness, context, None).await;
    hooks
}

fn model_request(provider: &str, session: &SessionID) -> ModelRequest {
    ModelRequest {
        session_id: session.clone(),
        agent: AgentID("build".to_owned()),
        model: ModelRef { id: "gpt-5.4".to_owned(), provider_id: ProviderID(provider.to_owned()), variant: None },
        kind: RequestKind::Primary,
        base_url: None,
        headers: IndexMap::new(),
    }
}

fn http_request(url: &str, headers: &[(&str, &str)], body: Value) -> HttpRequest {
    HttpRequest {
        session_id: SessionID("ses_test".to_owned()),
        agent: AgentID("build".to_owned()),
        model: ModelRef { id: "gpt-5.4".to_owned(), provider_id: provider_id(), variant: None },
        kind: RequestKind::Primary,
        request: PreparedHttp::new(
            "POST",
            url,
            headers.iter().map(|(name, value)| ((*name).to_owned(), (*value).to_owned())).collect(),
            Bytes::from(serde_json::to_vec(&body).expect("the request body serializes")),
        ),
    }
}

fn interaction_headers(value: &[(&str, &str)]) -> IndexMap<String, String> {
    value.iter().map(|(name, header)| ((*name).to_owned(), (*header).to_owned())).collect()
}

async fn resolve(hooks: &ProviderHooks, session: &SessionID, kind: RequestKind) -> Option<String> {
    let event = hooks
        .trigger_model_resolve(ModelResolve {
            session_id: session.clone(),
            agent: AgentID("build".to_owned()),
            kind,
            model: ModelRef { id: "auto".to_owned(), provider_id: provider_id(), variant: None },
        })
        .await;
    (event.model.id != "auto").then_some(event.model.id)
}

async fn token(hooks: &ProviderHooks, session: &SessionID, model_id: &str) -> Option<String> {
    let mut request = model_request(PROVIDER_ID, session);
    request.model.id = model_id.to_owned();
    hooks.trigger_model_request(request).await.headers.get("Copilot-Session-Token").cloned()
}

fn setting(model: &ModelInfo, key: &str) -> Option<String> {
    let (_, value) = model.settings.as_ref()?.rest.0.iter().find(|(name, _)| name == key)?;
    value.as_str().map(str::to_owned)
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

fn header(request: &PreparedHttp, name: &str) -> Option<String> {
    request.headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.clone())
}

async fn wait_for_model(harness: &Harness, id: &str) -> ModelInfo {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(model) = harness.services.model.get(&provider_id(), id).await.unwrap_or(None) {
                return model;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the model {id} did not appear"))
}

async fn wait_for_base_url(harness: &Harness, id: &str, expected: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let matches = harness
                .services
                .model
                .get(&provider_id(), id)
                .await
                .unwrap_or(None)
                .and_then(|model| setting(&model, "baseURL"))
                .is_some_and(|value| value == expected);
            if matches {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the model {id} did not get the base URL {expected}"));
}

async fn wait_until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if condition() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("the condition settles");
}

async fn seed_provider(harness: &Harness, models: Vec<Value>, enabled: bool) -> opencode_location::state::Registration {
    let id = provider_id();
    let models = decode_models(models);
    harness
        .services
        .provider
        .transform(move |editor| {
            editor.update(&id, |provider| {
                provider.package = String::new();
                if enabled {
                    provider.activation = ProviderInfoActivation::Enabled;
                }
            });
            if !models.is_empty() {
                editor.models_set(&id, &models);
            }
            Ok(())
        })
        .await
}

async fn register_copilot(harness: &mut Harness, context: PluginContext, interval: Option<Duration>) {
    let mut builtin = Builtin::default();
    github_copilot::register_with(&harness.services, &context, &mut builtin, Options { interval }).await;
    harness.record(builtin);
}

fn test_app() -> AppInfo {
    AppInfo { name: "test".to_owned(), version: "1.2.3".to_owned(), channel: "beta".to_owned() }
}

fn title_system() -> Vec<String> {
    vec!["You are a title generator.".to_owned()]
}

fn title_messages() -> Vec<UtilityTitleMessage> {
    vec![UtilityTitleMessage {
        role: "user".to_owned(),
        content: vec!["how do I make my python script faster".to_owned()],
    }]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn github_copilot_plugin_is_registered() {
    assert_eq!(ID, "opencode.provider.github.copilot");
    assert!(ORDER.iter().any(|slot| slot.id() == ID));
}

#[test]
fn prefers_the_account_specific_copilot_api_endpoint() {
    let metadata = JsonMap(vec![
        ("enterpriseUrl".to_owned(), json!("company.ghe.com")),
        ("apiEndpoint".to_owned(), json!("https://api.business.githubcopilot.com")),
    ]);
    assert_eq!(oauth::copilot_base_url(Some(&metadata)), "https://api.business.githubcopilot.com");
}

#[test]
fn rejects_accounts_without_copilot_chat_access() {
    let limited = oauth::User { chat_enabled: Some(false), can_signup_for_limited: Some(true), endpoints: None };
    assert!(
        oauth::copilot_entitlement_error(&limited).expect("the limited account is denied").contains("Copilot Free")
    );

    let seat = oauth::User { chat_enabled: Some(false), can_signup_for_limited: Some(false), endpoints: None };
    assert!(
        oauth::copilot_entitlement_error(&seat)
            .expect("the seatless account is denied")
            .contains("subscription or a seat")
    );

    let plain = oauth::User { chat_enabled: Some(false), can_signup_for_limited: None, endpoints: None };
    assert!(
        oauth::copilot_entitlement_error(&plain)
            .expect("the account without chat is denied")
            .contains("subscription or a seat")
    );
}

#[test]
fn only_blocks_on_an_explicit_entitlement_denial() {
    let enabled = oauth::User { chat_enabled: Some(true), can_signup_for_limited: None, endpoints: None };
    assert_eq!(oauth::copilot_entitlement_error(&enabled), None);

    let signup = oauth::User { chat_enabled: Some(true), can_signup_for_limited: Some(true), endpoints: None };
    assert_eq!(oauth::copilot_entitlement_error(&signup), None);

    let unknown = oauth::User { chat_enabled: None, can_signup_for_limited: None, endpoints: None };
    assert_eq!(oauth::copilot_entitlement_error(&unknown), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reloads_providers_only_when_a_refreshed_model_list_changes() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = seed_provider(&harness, Vec::new(), false).await;
    harness.credential(PROVIDER_ID, oauth(None)).await;

    let routes = Routes::new();
    routes.route_json("GET", PUBLIC_USER, json!({ "endpoints": { "api": format!("{BUSINESS_API}/") } }));
    routes.route_json("GET", BUSINESS_MODELS, sample_models());
    let context = plugin_context_with(&harness, routes.clone());
    register_copilot(&mut harness, context, Some(Duration::from_millis(300))).await;

    wait_until(|| routes.count("GET", BUSINESS_MODELS) >= 1).await;
    wait_for_model(&harness, "gpt-5").await;

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
    let seen = routes.count("GET", BUSINESS_MODELS);
    wait_until(|| routes.count("GET", BUSINESS_MODELS) > seen).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(folds.load(Ordering::SeqCst), before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refreshes_the_accounts_api_endpoint_instead_of_keeping_the_one_stored_at_login() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = seed_provider(&harness, Vec::new(), false).await;
    harness.credential(PROVIDER_ID, oauth(Some(json!({ "apiEndpoint": INDIVIDUAL_API })))).await;

    let routes = Routes::new();
    routes.route_json("GET", PUBLIC_USER, json!({ "endpoints": { "api": format!("{BUSINESS_API}/") } }));
    routes.route_json("GET", BUSINESS_MODELS, sample_models());
    let context = plugin_context_with(&harness, routes.clone());
    register_copilot(&mut harness, context, None).await;

    let model = wait_for_model(&harness, "gpt-5").await;
    assert_eq!(setting(&model, "baseURL").as_deref(), Some(BUSINESS_API));

    let requests = routes.requests();
    let urls: Vec<&str> = requests.iter().map(|request| request.url.as_str()).collect();
    assert_eq!(urls, [PUBLIC_USER, BUSINESS_MODELS]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keeps_the_stored_endpoint_when_the_account_lookup_fails() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = seed_provider(&harness, Vec::new(), false).await;
    harness.credential(PROVIDER_ID, oauth(Some(json!({ "apiEndpoint": INDIVIDUAL_API })))).await;

    let routes = Routes::new();
    routes.set_status("GET", PUBLIC_USER, 404);
    routes.route_json("GET", INDIVIDUAL_MODELS, sample_models());
    let context = plugin_context_with(&harness, routes.clone());
    register_copilot(&mut harness, context, None).await;

    wait_for_model(&harness, "gpt-5").await;
    let last = routes.requests().last().expect("the individual model list request").clone();
    assert_eq!(last.url, INDIVIDUAL_MODELS);
}

async fn pending_route(metadata: Value) -> (Harness, Arc<Routes>, opencode_location::state::Registration) {
    let mut harness = start(Input::new(Vec::new())).await;
    let seeded =
        seed_provider(&harness, vec![model("gpt-4o", "gpt-4o", Some(OPENAI_COMPATIBLE), "gpt-4o", PROVIDER_ID)], false)
            .await;
    harness.credential(PROVIDER_ID, oauth(Some(metadata))).await;

    let routes = Routes::new();
    routes.hold("GET", ENTERPRISE_USER);
    let context = plugin_context_with(&harness, routes.clone());
    register_copilot(&mut harness, context, None).await;
    tokio::time::timeout(Duration::from_secs(2), routes.arrived()).await.expect("the account lookup arrives");
    (harness, routes, seeded)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serves_an_enterprise_login_from_its_own_host_until_the_first_model_list_arrives() {
    let (harness, routes, _seeded) = pending_route(json!({ "enterpriseUrl": "company.ghe.com" })).await;
    wait_for_base_url(&harness, "gpt-4o", "https://copilot-api.company.ghe.com").await;

    let requests = routes.requests();
    let urls: Vec<&str> = requests.iter().map(|request| request.url.as_str()).collect();
    assert_eq!(urls, [ENTERPRISE_USER]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serves_a_login_from_the_endpoint_saved_with_it_until_the_first_model_list_arrives() {
    let (harness, routes, _seeded) = pending_route(json!({
        "enterpriseUrl": "company.ghe.com",
        "apiEndpoint": "https://api.tenant.ghe.com"
    }))
    .await;
    wait_for_base_url(&harness, "gpt-4o", "https://api.tenant.ghe.com").await;

    let requests = routes.requests();
    let urls: Vec<&str> = requests.iter().map(|request| request.url.as_str()).collect();
    assert_eq!(urls, [ENTERPRISE_USER]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registers_github_copilot_device_oauth() {
    let mut harness = start(Input::new(Vec::new())).await;
    let context = plugin_context_with(&harness, Routes::new());
    register_copilot(&mut harness, context, None).await;

    let integration = harness
        .services
        .integration
        .get(&integration_id())
        .await
        .expect("get the GitHub Copilot integration")
        .expect("the GitHub Copilot integration");
    let method = integration
        .methods
        .iter()
        .find_map(|method| match method {
            IntegrationMethod::Oauth(method) => Some(method),
            _ => None,
        })
        .expect("the device OAuth method");
    assert_eq!(method.id, IntegrationMethodID(oauth::METHOD_ID.to_owned()));
    assert_eq!(method.r#type, IntegrationOAuthMethodType::Oauth);
    assert_eq!(method.label, "Login with GitHub Copilot");
    let form = method.form.as_ref().expect("the device OAuth form");
    let keys: Vec<&str> = form
        .0
        .iter()
        .map(|field| match field {
            FormField1::String(field) => field.key.as_str(),
            _ => panic!("the device OAuth form has string fields"),
        })
        .collect();
    assert_eq!(keys, ["deploymentType", "enterpriseUrl"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removes_the_generic_key_method_and_reads_copilot_github_token_instead_of_github_token() {
    let mut harness = start(Input::new(Vec::new())).await;
    let id = integration_id();
    let _seeded = harness
        .services
        .integration
        .transform({
            let id = id.clone();
            move |editor| {
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
                        names: vec!["GITHUB_TOKEN".to_owned()],
                    }),
                });
                Ok(())
            }
        })
        .await;
    let context = plugin_context_with(&harness, Routes::new());
    register_copilot(&mut harness, context, None).await;

    let integration =
        harness.services.integration.get(&id).await.expect("get the GitHub Copilot integration").expect("it exists");
    let methods = integration.methods;
    assert_eq!(methods.len(), 2);
    let IntegrationMethod::Env(env) = &methods[0] else { panic!("the Copilot environment method is first") };
    assert_eq!(env.names, vec!["COPILOT_GITHUB_TOKEN".to_owned()]);
    let IntegrationMethod::Oauth(method) = &methods[1] else { panic!("the device OAuth method is second") };
    assert_eq!(method.id, IntegrationMethodID(oauth::METHOD_ID.to_owned()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn names_sessions_with_the_utility_model_as_a_free_background_request() {
    let routes = Routes::new();
    routes.route_json(
        "POST",
        &format!("{INDIVIDUAL_API}/chat/completions"),
        json!({ "choices": [{ "message": { "content": "  Speed Up Python Script\n" } }] }),
    );
    let app = test_app();
    let system = title_system();
    let messages = title_messages();
    let input = UtilityTitleInput { base_url: INDIVIDUAL_API, token: "token", model: "gpt-4o-mini", app: &app };
    let request =
        UtilityTitleRequest { session_id: "ses_title", system: &system, messages: &messages, max_tokens: Some(32.0) };

    let title =
        github_copilot::utility_title(routes.as_ref(), &input, &request).await.expect("the utility title is generated");
    assert_eq!(title, "Speed Up Python Script");

    let requests = routes.requests();
    assert_eq!(requests.len(), 1);
    let sent = &requests[0];
    assert_eq!(sent.url, format!("{INDIVIDUAL_API}/chat/completions"));
    assert_eq!(header(sent, "authorization").as_deref(), Some("Bearer token"));
    assert_eq!(header(sent, "x-interaction-type").as_deref(), Some("agent-session-name-generation"));
    assert_eq!(header(sent, "x-interaction-id").as_deref(), Some("ses_title"));
    assert_eq!(header(sent, "x-initiator").as_deref(), Some("agent"));
    assert_eq!(header(sent, "x-github-api-version").as_deref(), Some(oauth::API_VERSION));

    let body: Value = serde_json::from_slice(&sent.body).expect("the utility title request body");
    assert_eq!(body["model"], json!("gpt-4o-mini"));
    assert_eq!(body["stream"], json!(false));
    assert_eq!(body["max_tokens"].as_u64(), Some(32));
    assert_eq!(
        body["messages"],
        json!([
            { "role": "system", "content": "You are a title generator." },
            { "role": "user", "content": "how do I make my python script faster" }
        ])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fails_the_utility_title_on_rate_limits_so_the_billable_path_can_run() {
    let routes = Routes::new();
    routes.route_with("POST", &format!("{INDIVIDUAL_API}/chat/completions"), |_| Reply::status(429));
    let app = test_app();
    let system = title_system();
    let messages = title_messages();
    let input = UtilityTitleInput { base_url: INDIVIDUAL_API, token: "token", model: "gpt-4o-mini", app: &app };
    let request =
        UtilityTitleRequest { session_id: "ses_title", system: &system, messages: &messages, max_tokens: Some(32.0) };

    let error = github_copilot::utility_title(routes.as_ref(), &input, &request)
        .await
        .expect_err("the rate limit fails the utility title");
    assert!(error.to_string().contains("429"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fails_the_utility_title_on_an_empty_completion() {
    let routes = Routes::new();
    routes.route_json(
        "POST",
        &format!("{INDIVIDUAL_API}/chat/completions"),
        json!({ "choices": [{ "message": { "content": null } }] }),
    );
    let app = test_app();
    let system = title_system();
    let messages = title_messages();
    let input = UtilityTitleInput { base_url: INDIVIDUAL_API, token: "token", model: "gpt-4o-mini", app: &app };
    let request =
        UtilityTitleRequest { session_id: "ses_title", system: &system, messages: &messages, max_tokens: Some(32.0) };

    let error = github_copilot::utility_title(routes.as_ref(), &input, &request)
        .await
        .expect_err("the empty completion fails the utility title");
    assert!(error.to_string().contains("empty"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn routes_models_dev_fallback_models_to_native_copilot_apis() {
    let mut harness = start(Input::new(Vec::new())).await;
    let seeded = vec![
        model("claude-sonnet-4.5", "claude-sonnet-4.5", Some(ANTHROPIC), "claude-sonnet-4.5", PROVIDER_ID),
        model("gpt-4.1", "gpt-4.1", Some(OPENAI), "gpt-4.1", PROVIDER_ID),
        model("gpt-5", "gpt-5", Some(OPENAI_COMPATIBLE), "gpt-5", PROVIDER_ID),
        model("gpt-5.1-codex", "gpt-5.1-codex", Some(OPENAI_COMPATIBLE), "gpt-5.1-codex", PROVIDER_ID),
        model("gpt-5-mini", "gpt-5-mini", Some(OPENAI_COMPATIBLE), "gpt-5-mini", PROVIDER_ID),
        model("gpt-4o", "gpt-4o", Some(OPENAI_COMPATIBLE), "gpt-4o", PROVIDER_ID),
        model("grok-4.5", "grok-4.5", Some(OPENAI_COMPATIBLE), "grok-4.5", PROVIDER_ID),
        model("mai-code-1.1-flash", "mai-code-1.1-flash", Some(OPENAI_COMPATIBLE), "mai-code-1.1-flash", PROVIDER_ID),
        model("gemini-3.8-flash", "gemini-3.8-flash", Some(OPENAI_COMPATIBLE), "gemini-3.8-flash", PROVIDER_ID),
        model("small", "gpt-5-mini", Some(OPENAI_COMPATIBLE), "small", PROVIDER_ID),
    ];
    let _seeded = seed_provider(&harness, seeded, true).await;
    let context = plugin_context_with(&harness, Routes::new());
    register_copilot(&mut harness, context, None).await;

    wait_for_base_url(&harness, "claude-sonnet-4.5", &format!("{COPILOT_API}/v1")).await;

    let expected = [
        ("claude-sonnet-4.5", ANTHROPIC, format!("{COPILOT_API}/v1")),
        ("gpt-4.1", OPENAI_RESPONSES, COPILOT_API.to_owned()),
        ("gpt-5", OPENAI_RESPONSES, COPILOT_API.to_owned()),
        ("gpt-5.1-codex", OPENAI_RESPONSES, COPILOT_API.to_owned()),
        ("grok-4.5", OPENAI_RESPONSES, COPILOT_API.to_owned()),
        ("mai-code-1.1-flash", OPENAI_RESPONSES, COPILOT_API.to_owned()),
        ("gpt-5-mini", OPENAI_COMPATIBLE, COPILOT_API.to_owned()),
        ("small", OPENAI_COMPATIBLE, COPILOT_API.to_owned()),
        ("gpt-4o", OPENAI_COMPATIBLE, COPILOT_API.to_owned()),
        ("gemini-3.8-flash", OPENAI_COMPATIBLE, COPILOT_API.to_owned()),
    ];
    for (id, package, base) in expected {
        let model = harness.services.model.get(&provider_id(), id).await.unwrap().expect("the routed model");
        assert_eq!(model.package.as_deref(), Some(package), "{id}");
        assert_eq!(setting(&model, "baseURL").as_deref(), Some(base.as_str()), "{id}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disables_gpt_5_chat_latest_before_copilot_language_selection() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = seed_provider(
        &harness,
        vec![model(
            "gpt-5-chat-latest",
            "gpt-5-chat-latest",
            Some(OPENAI_COMPATIBLE),
            "gpt-5-chat-latest",
            PROVIDER_ID,
        )],
        true,
    )
    .await;
    let context = plugin_context_with(&harness, Routes::new());
    register_copilot(&mut harness, context, None).await;

    let model = harness.services.model.get(&provider_id(), "gpt-5-chat-latest").await.unwrap().expect("the chat model");
    assert!(!model.enabled);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn does_not_disable_gpt_5_chat_latest_for_non_copilot_providers() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = harness
        .services
        .provider
        .transform(|editor| {
            let id = ProviderID("custom-copilot".to_owned());
            editor.update(&id, |provider| provider.activation = ProviderInfoActivation::Enabled);
            editor.models_update(&id, "gpt-5-chat-latest", |_| {});
            Ok(())
        })
        .await;
    let context = plugin_context_with(&harness, Routes::new());
    register_copilot(&mut harness, context, None).await;

    let model = harness
        .services
        .model
        .get(&ProviderID("custom-copilot".to_owned()), "gpt-5-chat-latest")
        .await
        .unwrap()
        .expect("the custom chat model");
    assert!(model.enabled);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn defensively_syncs_advertised_copilot_models() {
    let base = "https://copilot.test";
    let sender = RecordedSender::ordered(vec![Exchange::new(
        "GET",
        &format!("{base}/models"),
        200,
        json!({
            "data": [
                {
                    "model_picker_enabled": true,
                    "id": "gpt-5",
                    "name": "GPT-5 remote",
                    "version": "gpt-5-2026-06-01",
                    "supported_endpoints": ["/responses"],
                    "billing": {
                        "token_prices": {
                            "batch_size": 0,
                            "default": { "input_price": 10, "output_price": 20, "cache_price": 5 }
                        }
                    },
                    "capabilities": {
                        "family": "gpt",
                        "limits": {
                            "max_context_window_tokens": 200000,
                            "max_output_tokens": 16384,
                            "max_prompt_tokens": 180000,
                            "vision": {
                                "max_prompt_image_size": 10000000,
                                "max_prompt_images": 10,
                                "supported_media_types": ["image/png", "application/pdf"]
                            }
                        },
                        "supports": { "tool_calls": true, "vision": true, "reasoning_effort": ["low", "high"] }
                    }
                },
                {
                    "model_picker_enabled": true,
                    "id": "claude-sonnet",
                    "name": "Claude Sonnet",
                    "version": "claude-sonnet-2026-06-01",
                    "supported_endpoints": ["/v1/messages"],
                    "capabilities": {
                        "family": "claude",
                        "limits": { "max_output_tokens": 16384, "max_prompt_tokens": 180000 },
                        "supports": { "tool_calls": true }
                    }
                },
                {
                    "model_picker_enabled": true,
                    "id": "vision-only",
                    "name": "Vision only",
                    "version": "vision-only-2026-06-01",
                    "capabilities": {
                        "family": "vision",
                        "limits": {
                            "max_output_tokens": 16384,
                            "max_prompt_tokens": 180000,
                            "vision": {
                                "max_prompt_image_size": 10000000,
                                "max_prompt_images": 10,
                                "supported_media_types": ["image/png"]
                            }
                        },
                        "supports": { "tool_calls": true, "vision": true }
                    }
                },
                {
                    "model_picker_enabled": false,
                    "id": "utility",
                    "name": "Utility",
                    "version": "utility-2026-06-01",
                    "capabilities": {
                        "family": "utility",
                        "limits": { "max_output_tokens": 1000, "max_prompt_tokens": 8000 },
                        "supports": { "tool_calls": false }
                    }
                },
                { "model_picker_enabled": true, "id": "incomplete" }
            ]
        })
        .to_string(),
    )]);

    let existing = decode_models(vec![
        model("gpt-5", "gpt-5", None, "GPT-5 local", PROVIDER_ID),
        model("stale", "stale", None, "stale", PROVIDER_ID),
    ]);
    let discovered = models::get(&sender, base, Vec::new(), &existing).await.expect("the advertised models");

    let gpt = discovered.get("gpt-5").expect("the gpt-5 model");
    assert_eq!(gpt.name, "GPT-5 local");
    assert_eq!(gpt.package.as_deref(), Some(OPENAI_RESPONSES));
    assert_eq!(model_settings(gpt), json!({ "baseURL": base }));
    assert_eq!(gpt.cost[0].input.0, 0.0);
    assert_eq!(gpt.cost[0].output.0, 0.0);
    assert_eq!(gpt.cost[0].cache.read.0, 0.0);
    assert_eq!(gpt.cost[0].cache.write.0, 0.0);
    let variants: Vec<&str> = gpt.variants.iter().map(|variant| variant.id.as_str()).collect();
    assert_eq!(variants, ["low", "high"]);
    let input: Vec<&str> = gpt.capabilities.input.iter().map(String::as_str).collect();
    assert_eq!(input, ["text", "image", "pdf"]);

    let claude = discovered.get("claude-sonnet").expect("the claude-sonnet model");
    assert_eq!(claude.package.as_deref(), Some(ANTHROPIC));
    assert_eq!(model_settings(claude), json!({ "baseURL": format!("{base}/v1") }));

    let vision = discovered.get("vision-only").expect("the vision-only model");
    let input: Vec<&str> = vision.capabilities.input.iter().map(String::as_str).collect();
    assert_eq!(input, ["text", "image"]);

    assert!(!discovered.get("utility").expect("the utility model").enabled);
    assert!(!discovered.contains_key("stale"));
    assert!(!discovered.contains_key("incomplete"));
}

fn priced_model(id: &str, cache: Value) -> Value {
    let mut default = serde_json::Map::new();
    default.insert("input_price".to_owned(), json!(250));
    default.insert("output_price".to_owned(), json!(1500));
    if let Value::Object(cache) = cache {
        default.extend(cache);
    }
    json!({
        "model_picker_enabled": true,
        "id": id,
        "name": id,
        "version": format!("{id}-2026-08-01"),
        "supported_endpoints": ["/chat/completions"],
        "billing": { "token_prices": { "batch_size": 1_000_000, "default": Value::Object(default) } },
        "capabilities": {
            "family": "gpt",
            "limits": { "max_output_tokens": 1000, "max_prompt_tokens": 8000 },
            "supports": { "tool_calls": true }
        }
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prices_cache_reads_from_either_token_price_spelling() {
    let base = "https://copilot.test";
    let sender = RecordedSender::ordered(vec![Exchange::new(
        "GET",
        &format!("{base}/models"),
        200,
        json!({
            "data": [
                priced_model("renamed", json!({ "cache_read_price": 25, "cache_write_price": 0 })),
                priced_model("legacy", json!({ "cache_price": 25 })),
                priced_model("unpriced", json!({}))
            ]
        })
        .to_string(),
    )]);
    let discovered = models::get(&sender, base, Vec::new(), &[]).await.expect("the priced models");

    for id in ["renamed", "legacy"] {
        let model = discovered.get(id).expect("the priced model");
        assert_eq!(model.cost[0].input.0, 2.5, "{id}");
        assert_eq!(model.cost[0].output.0, 15.0, "{id}");
        assert!((model.cost[0].cache.read.0 - 0.25).abs() < 1e-9, "{id}");
    }
    assert_eq!(discovered.get("unpriced").expect("the unpriced model").cost[0].cache.read.0, 0.0);
}

#[test]
fn keys_the_shared_cache_by_connection_url_and_fingerprinted_headers() {
    let connection = ConnectionInfo::Credential(ConnectionCredentialInfo {
        r#type: ConnectionCredentialInfoType::Credential,
        id: CredentialID("cred_1".to_owned()),
        label: "first".to_owned(),
        method: ConnectionCredentialInfoMethod::Oauth,
        status: None,
    });
    let sorted = github_copilot::models_key(
        Some(&connection),
        "https://api.githubcopilot.com",
        &[("a".to_owned(), "1".to_owned()), ("b".to_owned(), "2".to_owned())],
    );
    let reversed = github_copilot::models_key(
        Some(&connection),
        "https://api.githubcopilot.com",
        &[("b".to_owned(), "2".to_owned()), ("a".to_owned(), "1".to_owned())],
    );
    assert_eq!(sorted, reversed);
    assert!(sorted.starts_with("github-copilot:credential:cred_1:https://api.githubcopilot.com:"));
    assert_ne!(
        sorted,
        github_copilot::models_key(
            Some(&connection),
            "https://api.githubcopilot.com",
            &[("a".to_owned(), "1".to_owned()), ("b".to_owned(), "3".to_owned())],
        )
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exchanges_enterprise_device_flow_credentials() {
    let routes = Routes::new();
    routes.route_json(
        "POST",
        "https://company.ghe.com/login/device/code",
        json!({
            "verification_uri": "https://company.ghe.com/login/device",
            "user_code": "ABCD-1234",
            "device_code": "dev",
            "interval": 5
        }),
    );
    routes.route_json("POST", "https://company.ghe.com/login/oauth/access_token", json!({ "access_token": "access" }));
    routes.route_json(
        "GET",
        "https://api.company.ghe.com/copilot_internal/user",
        json!({ "chat_enabled": true, "endpoints": { "api": "https://api.tenant.ghe.com/" } }),
    );

    let implementation = oauth::CopilotOAuth::new(routes.clone(), "opencode/test/1.0".to_owned());
    let answer = JsonMap(vec![
        ("deploymentType".to_owned(), FormValue::String("enterprise".to_owned())),
        ("enterpriseUrl".to_owned(), FormValue::String("https://company.ghe.com/".to_owned())),
    ]);
    let authorization = implementation.authorize(answer).await.expect("the enterprise device authorization");
    assert_eq!(authorization.url, "https://company.ghe.com/login/device");
    assert_eq!(authorization.instructions, "Enter code: ABCD-1234");
    let OAuthMode::Auto { callback, .. } = authorization.mode else { panic!("the enterprise flow is automatic") };
    let credential = callback().await.expect("the enterprise device credential");
    assert_eq!(credential.access, "access");
    assert_eq!(credential.refresh, "access");
    assert_eq!(credential.expires, 0.0);
    let metadata = credential.metadata.expect("the enterprise credential metadata").0;
    assert_eq!(
        metadata.iter().find(|(key, _)| key == "enterpriseUrl").map(|(_, value)| value),
        Some(&json!("company.ghe.com"))
    );
    assert_eq!(
        metadata.iter().find(|(key, _)| key == "apiEndpoint").map(|(_, value)| value),
        Some(&json!("https://api.tenant.ghe.com"))
    );

    let requests = routes.requests();
    let urls: Vec<&str> = requests.iter().map(|request| request.url.as_str()).collect();
    assert_eq!(
        urls,
        [
            "https://company.ghe.com/login/device/code",
            "https://company.ghe.com/login/oauth/access_token",
            "https://api.company.ghe.com/copilot_internal/user"
        ]
    );

    let device: Value = serde_json::from_slice(&requests[0].body).expect("the device code request body");
    assert_eq!(device["client_id"], json!(oauth::CLIENT_ID));
    assert_eq!(device["scope"], json!("read:user"));

    let token: Value = serde_json::from_slice(&requests[1].body).expect("the token request body");
    assert_eq!(token["grant_type"], json!("urn:ietf:params:oauth:grant-type:device_code"));
    assert_eq!(token["device_code"], json!("dev"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adds_copilot_authentication_and_request_metadata_headers_to_openai_requests() {
    let mut harness = start(Input::new(Vec::new())).await;
    let hooks = register_hooks(&mut harness, FakeSessions::default(), Routes::new()).await;

    let event = hooks
        .trigger_http_request(http_request(
            &format!("{COPILOT_API}/chat/completions"),
            &[("content-type", "application/json"), ("authorization", "Bearer token")],
            json!({
                "messages": [{
                    "role": "user",
                    "content": [{ "type": "image_url", "image_url": { "url": "data:image/png" } }]
                }]
            }),
        ))
        .await;

    assert_eq!(header(&event.request, "authorization").as_deref(), Some("Bearer token"));
    assert_eq!(header(&event.request, "x-initiator").as_deref(), Some("user"));
    assert_eq!(header(&event.request, "copilot-vision-request").as_deref(), Some("true"));
    assert_eq!(header(&event.request, "x-github-api-version").as_deref(), Some(oauth::API_VERSION));
    assert_eq!(header(&event.request, "openai-intent").as_deref(), Some("conversation-edits"));
    assert_eq!(header(&event.request, "anthropic-beta"), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adds_copilot_authentication_to_native_anthropic_requests() {
    let mut harness = start(Input::new(Vec::new())).await;
    let hooks = register_hooks(&mut harness, FakeSessions::default(), Routes::new()).await;

    let event = hooks
        .trigger_http_request(http_request(
            &format!("{COPILOT_API}/v1/messages"),
            &[
                ("content-type", "application/json"),
                ("x-api-key", "token"),
                ("anthropic-beta", "interleaved-thinking-2025-05-14,thinking-binding-controls-2026-08-01"),
            ],
            json!({
                "messages": [{ "role": "user", "content": [{ "type": "text", "text": "hello" }] }]
            }),
        ))
        .await;

    assert_eq!(header(&event.request, "authorization").as_deref(), Some("Bearer token"));
    assert_eq!(header(&event.request, "x-api-key"), None);
    assert_eq!(header(&event.request, "x-initiator").as_deref(), Some("user"));
    assert_eq!(
        header(&event.request, "anthropic-beta").as_deref(),
        Some("interleaved-thinking-2025-05-14,thinking-binding-controls-2026-08-01")
    );
    assert_eq!(header(&event.request, "x-github-api-version").as_deref(), Some(oauth::API_VERSION));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flags_images_returned_by_tools_in_responses_requests() {
    let mut harness = start(Input::new(Vec::new())).await;
    let hooks = register_hooks(&mut harness, FakeSessions::default(), Routes::new()).await;

    let event = hooks
        .trigger_http_request(http_request(
            &format!("{COPILOT_API}/responses"),
            &[("authorization", "Bearer token")],
            json!({
                "input": [
                    { "role": "user", "content": [{ "type": "input_text", "text": "Take a screenshot." }] },
                    { "type": "function_call", "call_id": "call_1", "name": "screenshot", "arguments": "{}" },
                    {
                        "type": "function_call_output",
                        "call_id": "call_1",
                        "output": [{ "type": "input_image", "image_url": "data:image/png;base64,AAAA" }]
                    }
                ]
            }),
        ))
        .await;

    assert_eq!(header(&event.request, "copilot-vision-request").as_deref(), Some("true"));
    assert_eq!(header(&event.request, "x-initiator").as_deref(), Some("agent"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn treats_tool_images_moved_into_a_chat_completions_user_message_as_agent_initiated() {
    let mut harness = start(Input::new(Vec::new())).await;
    let hooks = register_hooks(&mut harness, FakeSessions::default(), Routes::new()).await;

    let event = hooks
        .trigger_http_request(http_request(
            &format!("{COPILOT_API}/chat/completions"),
            &[("authorization", "Bearer token")],
            json!({
                "messages": [
                    { "role": "user", "content": "Take a screenshot." },
                    {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call_1",
                            "type": "function",
                            "function": { "name": "screenshot", "arguments": "{}" }
                        }]
                    },
                    { "role": "tool", "tool_call_id": "call_1", "content": "" },
                    { "role": "user", "content": [{ "type": "image_url", "image_url": { "url": "data:image/png;base64,AAAA" } }] }
                ]
            }),
        ))
        .await;

    assert_eq!(header(&event.request, "x-initiator").as_deref(), Some("agent"));
    assert_eq!(header(&event.request, "copilot-vision-request").as_deref(), Some("true"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keeps_a_declared_agent_initiator_when_the_body_looks_user_initiated() {
    let mut harness = start(Input::new(Vec::new())).await;
    let hooks = register_hooks(&mut harness, FakeSessions::default(), Routes::new()).await;

    let event = hooks
        .trigger_http_request(http_request(
            &format!("{COPILOT_API}/responses"),
            &[("authorization", "Bearer token"), ("x-initiator", "agent")],
            json!({
                "input": [{ "role": "user", "content": [{ "type": "input_text", "text": "summarize" }] }]
            }),
        ))
        .await;

    assert_eq!(header(&event.request, "x-initiator").as_deref(), Some("agent"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn classifies_main_loop_steps_as_agent_interactions() {
    let (sessions, parent, _child) = session_fixture();
    let mut harness = start(Input::new(Vec::new())).await;
    let hooks = register_hooks(&mut harness, sessions, Routes::new()).await;

    let event = hooks.trigger_model_request(model_request(PROVIDER_ID, &parent)).await;

    assert_eq!(
        event.headers,
        interaction_headers(&[("X-Interaction-Type", "conversation-agent"), ("X-Interaction-Id", "ses_parent")])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn classifies_child_session_steps_as_subagent_interactions() {
    let (sessions, _parent, child) = session_fixture();
    let mut harness = start(Input::new(Vec::new())).await;
    let hooks = register_hooks(&mut harness, sessions, Routes::new()).await;

    let event = hooks.trigger_model_request(model_request(PROVIDER_ID, &child)).await;

    assert_eq!(
        event.headers,
        interaction_headers(&[
            ("X-Interaction-Type", "conversation-subagent"),
            ("X-Interaction-Id", "ses_parent"),
            ("x-initiator", "agent"),
        ])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn classifies_title_generation_as_a_background_interaction() {
    let (sessions, parent, _child) = session_fixture();
    let mut harness = start(Input::new(Vec::new())).await;
    let hooks = register_hooks(&mut harness, sessions, Routes::new()).await;

    let mut request = model_request(PROVIDER_ID, &parent);
    request.kind = RequestKind::Title;
    let event = hooks.trigger_model_request(request).await;

    assert_eq!(
        event.headers,
        interaction_headers(&[
            ("X-Interaction-Type", "conversation-background"),
            ("X-Interaction-Id", "ses_parent"),
            ("x-initiator", "agent"),
        ])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn classifies_compaction_requests_by_kind_rather_than_agent() {
    let (sessions, _parent, child) = session_fixture();
    let mut harness = start(Input::new(Vec::new())).await;
    let hooks = register_hooks(&mut harness, sessions, Routes::new()).await;

    let mut request = model_request(PROVIDER_ID, &child);
    request.kind = RequestKind::Compaction;
    let event = hooks.trigger_model_request(request).await;

    assert_eq!(
        event.headers,
        interaction_headers(&[
            ("X-Interaction-Type", "conversation-compaction"),
            ("X-Interaction-Id", "ses_parent"),
            ("x-initiator", "agent"),
        ])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn does_not_classify_by_agent_name() {
    let (sessions, parent, _child) = session_fixture();
    let mut harness = start(Input::new(Vec::new())).await;
    let hooks = register_hooks(&mut harness, sessions, Routes::new()).await;

    let mut request = model_request(PROVIDER_ID, &parent);
    request.agent = AgentID("compaction".to_owned());
    let event = hooks.trigger_model_request(request).await;

    assert_eq!(
        event.headers,
        interaction_headers(&[("X-Interaction-Type", "conversation-agent"), ("X-Interaction-Id", "ses_parent")])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ignores_other_providers_model_requests() {
    let (sessions, parent, _child) = session_fixture();
    let mut harness = start(Input::new(Vec::new())).await;
    let hooks = register_hooks(&mut harness, sessions, Routes::new()).await;

    let event = hooks.trigger_model_request(model_request("openai", &parent)).await;

    assert!(event.headers.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn classifies_session_generation_requests_as_agent_interactions() {
    let (sessions, parent, _child) = session_fixture();
    let mut harness = start(Input::new(Vec::new())).await;
    let hooks = register_hooks(&mut harness, sessions, Routes::new()).await;

    let mut request = model_request(PROVIDER_ID, &parent);
    request.kind = RequestKind::Generate;
    let event = hooks.trigger_model_request(request).await;

    assert_eq!(
        event.headers,
        interaction_headers(&[("X-Interaction-Type", "conversation-agent"), ("X-Interaction-Id", "ses_parent")])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sends_the_utility_title_request_through_the_hosts_fetch() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = seed_provider(&harness, Vec::new(), false).await;
    harness.credential(PROVIDER_ID, oauth(None)).await;

    let routes = Routes::new();
    routes.route_json("GET", PUBLIC_USER, json!({}));
    routes.route_json(
        "GET",
        &format!("{COPILOT_API}/models"),
        json!({
            "data": [{
                "model_picker_enabled": true,
                "id": "gpt-4o-mini",
                "name": "GPT-4o mini",
                "version": "gpt-4o-mini",
                "capabilities": {
                    "family": "gpt-4o-mini",
                    "limits": { "max_output_tokens": 1, "max_prompt_tokens": 1 },
                    "supports": { "tool_calls": true }
                }
            }]
        }),
    );
    routes.route_json(
        "POST",
        &format!("{COPILOT_API}/chat/completions"),
        json!({ "choices": [{ "message": { "content": "Faster Python" } }] }),
    );
    let hooks = register_hooks(&mut harness, FakeSessions::default(), routes.clone()).await;
    wait_for_model(&harness, "gpt-4o-mini").await;

    let event = hooks
        .trigger_title(Title {
            session_id: SessionID("ses_title".to_owned()),
            agent: AgentID("title".to_owned()),
            model: ModelRef { id: "gpt-5".to_owned(), provider_id: provider_id(), variant: None },
            kind: RequestKind::Title,
            system: title_system(),
            messages: title_messages(),
            max_tokens: Some(32.0),
            result: None,
        })
        .await;

    assert_eq!(event.result.as_deref(), Some("Faster Python"));
    assert_eq!(routes.count("POST", &format!("{COPILOT_API}/chat/completions")), 1);
    assert_eq!(routes.count("POST", &format!("{COPILOT_API}/v1/chat/completions")), 0);
}

fn remote_model(id: &str, extra: Value) -> Value {
    let mut value = json!({
        "model_picker_enabled": false,
        "id": id,
        "name": id,
        "version": id,
        "supported_endpoints": ["/chat/completions"],
        "capabilities": {
            "family": id,
            "limits": { "max_prompt_tokens": 128000, "max_output_tokens": 64000 },
            "supports": { "tool_calls": true }
        }
    });
    if let Value::Object(extra) = extra {
        value.as_object_mut().expect("the remote model template is an object").extend(extra);
    }
    value
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn routes_auto_through_copilots_router_and_keeps_the_pick_until_compaction() {
    let base = "https://copilot.test";
    let pick = Arc::new(Mutex::new("routed".to_owned()));
    let available = Arc::new(AtomicBool::new(true));

    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = seed_provider(&harness, Vec::new(), false).await;
    harness.credential(PROVIDER_ID, oauth(Some(json!({ "apiEndpoint": base })))).await;

    let routes = Routes::new();
    routes.set_status("GET", PUBLIC_USER, 404);
    routes.route_json(
        "GET",
        &format!("{base}/models"),
        json!({
            "data": [
                remote_model("routed", json!({ "billing": { "auto_discount": 0.1 } })),
                remote_model("other", json!({ "billing": { "auto_discount": 0.1 } })),
                remote_model("fallback", json!({ "is_chat_fallback": true }))
            ]
        }),
    );
    routes.route_with("POST", &format!("{base}/auto"), {
        let pick = pick.clone();
        let available = available.clone();
        move |_| {
            if !available.load(Ordering::SeqCst) {
                return Reply::status(503);
            }
            let selected = pick.lock().expect("the auto pick lock is never poisoned").clone();
            Reply::json(json!({
                "session_token": format!("token-{selected}"),
                "expires_at": 4_000_000_000.0,
                "selected_model": { "id": selected }
            }))
        }
    });
    let (sessions, parent, child) = session_fixture();
    let hooks = register_hooks(&mut harness, sessions, routes.clone()).await;
    wait_for_model(&harness, "auto").await;

    let models = harness.services.model.available(None).await.expect("the available models");
    let ids: Vec<&str> =
        models.iter().filter(|model| model.provider_id == provider_id()).map(|model| model.id.as_str()).collect();
    assert_eq!(ids, ["auto"]);

    let auto = format!("{base}/auto");
    assert_eq!(resolve(&hooks, &parent, RequestKind::Title).await, None);
    assert_eq!(routes.count("POST", &auto), 0);

    assert_eq!(resolve(&hooks, &parent, RequestKind::Primary).await.as_deref(), Some("routed"));
    assert_eq!(resolve(&hooks, &parent, RequestKind::Primary).await.as_deref(), Some("routed"));
    assert_eq!(resolve(&hooks, &parent, RequestKind::Generate).await.as_deref(), Some("routed"));
    assert_eq!(routes.count("POST", &auto), 1);
    let first = routes
        .requests()
        .into_iter()
        .find(|request| request.method == "POST" && request.url == auto)
        .expect("the auto request");
    let body: Value = serde_json::from_slice(&first.body).expect("the auto request body");
    assert_eq!(body, json!({ "prompt": "" }));

    assert_eq!(token(&hooks, &parent, "routed").await.as_deref(), Some("token-routed"));
    assert_eq!(token(&hooks, &parent, "other").await, None);

    *pick.lock().expect("the auto pick lock is never poisoned") = "other".to_owned();
    let _compacted = hooks
        .trigger_compaction(Compaction {
            session_id: parent.clone(),
            agent: AgentID("compaction".to_owned()),
            model: ModelRef { id: "routed".to_owned(), provider_id: provider_id(), variant: None },
            kind: RequestKind::Primary,
            options: IndexMap::new(),
            result: None,
        })
        .await;
    assert_eq!(token(&hooks, &parent, "routed").await.as_deref(), Some("token-routed"));

    assert_eq!(resolve(&hooks, &parent, RequestKind::Primary).await.as_deref(), Some("other"));
    assert_eq!(token(&hooks, &parent, "other").await.as_deref(), Some("token-other"));
    assert_eq!(routes.count("POST", &auto), 2);
    let last = routes
        .requests()
        .into_iter()
        .filter(|request| request.method == "POST" && request.url == auto)
        .last()
        .expect("the auto request");
    assert_eq!(header(&last, "authorization").as_deref(), Some("Bearer token"));

    *pick.lock().expect("the auto pick lock is never poisoned") = "routed".to_owned();
    harness.credential(PROVIDER_ID, oauth_with("second", Some(json!({ "apiEndpoint": base })))).await;
    wait_until(|| routes.count("GET", &format!("{base}/models")) >= 2).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(resolve(&hooks, &parent, RequestKind::Primary).await.as_deref(), Some("routed"));
    let last = routes
        .requests()
        .into_iter()
        .filter(|request| request.method == "POST" && request.url == auto)
        .last()
        .expect("the auto request");
    assert_eq!(header(&last, "authorization").as_deref(), Some("Bearer second"));

    available.store(false, Ordering::SeqCst);
    assert_eq!(resolve(&hooks, &child, RequestKind::Primary).await.as_deref(), Some("fallback"));
}
