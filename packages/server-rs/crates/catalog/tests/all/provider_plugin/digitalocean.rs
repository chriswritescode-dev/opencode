use std::sync::LazyLock;
use std::time::Duration;

use opencode_catalog::builtin::Builtin;
use opencode_catalog::credential::{CreateInput, Info, Update};
use opencode_catalog::integration::{Attempt, AttemptMode, AttemptStatus, Implementation, OAuthConnectInput};
use opencode_catalog::provider_plugin::digitalocean::{self, Options};
use opencode_contract::generated::{
    CredentialKey, CredentialKeyType, CredentialOAuth, CredentialOAuthType, CredentialValue, IntegrationEnvMethod,
    IntegrationEnvMethodType, IntegrationID, IntegrationKeyMethod, IntegrationKeyMethodType, IntegrationMethod,
    IntegrationMethodID, ModelInfo, ProviderID,
};
use serde_json::{Value, json};

use crate::builtin::{Harness, Input, start};
use crate::provider_plugin::support::{Routes, plugin_context_with, provider_settings};

const PROVIDER_ID: &str = "digitalocean";

const OPENAI_COMPATIBLE: &str = "@opencode/ai/providers/openai-compatible";

const BASE_URL: &str = "https://inference.do-ai.run/v1";

const ROUTERS: &str = "https://api.digitalocean.com/v2/gen-ai/models/routers";

static PORT: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));

fn provider_id() -> ProviderID {
    ProviderID(PROVIDER_ID.to_owned())
}

fn integration_id() -> IntegrationID {
    IntegrationID(PROVIDER_ID.to_owned())
}

fn method_id() -> IntegrationMethodID {
    IntegrationMethodID("browser".to_owned())
}

fn oauth_value(access: &str, expires: f64) -> CredentialValue {
    CredentialValue::Oauth(CredentialOAuth {
        r#type: CredentialOAuthType::Oauth,
        method_id: method_id(),
        refresh: String::new(),
        access: access.to_owned(),
        expires,
        metadata: None,
    })
}

fn key_value(key: &str) -> CredentialValue {
    CredentialValue::Key(CredentialKey { r#type: CredentialKeyType::Key, key: key.to_owned(), metadata: None })
}

fn create_credential(harness: &Harness, value: CredentialValue, activate: Option<bool>) -> Info {
    harness
        .services
        .credentials
        .create(CreateInput { id: None, integration_id: integration_id(), value, label: None, activate })
        .expect("create the DigitalOcean credential")
}

async fn add_plugin(harness: &mut Harness, routes: std::sync::Arc<Routes>, options: Options) {
    let context = plugin_context_with(harness, routes);
    let mut builtin = Builtin::default();
    digitalocean::register_with(&harness.services, &context, &mut builtin, options).await;
    harness.record(builtin);
}

async fn seed(harness: &Harness) -> opencode_location::state::Registration {
    let id = provider_id();
    let settings = provider_settings(BASE_URL);
    harness
        .services
        .provider
        .transform(move |editor| {
            editor.update(&id, |provider| {
                provider.package = OPENAI_COMPATIBLE.to_owned();
                provider.settings = Some(settings.clone());
            });
            editor.models_update(&id, "snapshot-model", |_| {});
            editor.models_update(&id, "router:configured", |model| {
                model.name = "Configured router".to_owned();
                model.limit.context = 256_000.0;
            });
            editor.models_update(&ProviderID("openai".to_owned()), "router:alpha", |_| {});
            Ok(())
        })
        .await
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
        .expect("connect the DigitalOcean integration")
}

async fn cancel(harness: &Harness, attempt: &Attempt) {
    harness.services.integration.oauth().cancel(&integration_id(), &attempt.attempt_id).await;
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

async fn wait_for_model_absent(harness: &Harness, id: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if harness.services.model.get(&provider_id(), id).await.unwrap_or(None).is_none() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the model {id} did not disappear"))
}

async fn wait_for_requests(routes: &Routes, count: usize) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if routes.requests().len() >= count {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the discovery requests did not arrive"))
}

async fn wait_for_authorization(routes: &Routes, value: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if routes.requests().iter().any(|request| authorization(request).as_deref() == Some(value)) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the authorization {value} did not arrive"))
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
                .expect("the DigitalOcean attempt status");
            if !matches!(status, AttemptStatus::Pending { .. }) {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the DigitalOcean attempt leaves pending")
}

fn authorization(request: &opencode_ai::route::executor::PreparedHttp) -> Option<String> {
    request.headers.iter().find(|(name, _)| name.eq_ignore_ascii_case("authorization")).map(|(_, value)| value.clone())
}

fn header(request: &opencode_ai::route::executor::PreparedHttp, name: &str) -> Option<String> {
    request.headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.clone())
}

fn param(url: &url::Url, name: &str) -> Option<String> {
    url.query_pairs().find(|(key, _)| key == name).map(|(_, value)| value.into_owned())
}

fn routers(names: &[&str]) -> Value {
    json!({ "model_routers": names.iter().map(|name| json!({ "name": name })).collect::<Vec<_>>() })
}

fn model_settings(model: &ModelInfo) -> Option<Value> {
    let settings = model.settings.as_ref()?;
    Some(serde_json::from_str(&opencode_contract::codec::encode_json(settings)).expect("the model settings json"))
}

async fn rejects_callback(body: &str, message: &str) {
    let _guard = PORT.lock().await;
    let mut harness = start(Input::new(Vec::new())).await;
    add_plugin(&mut harness, Routes::new(), Options::default()).await;
    let attempt = connect(&harness).await;
    let response = opencode_config::http::client()
        .post("http://localhost:1456/auth/token")
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.to_owned())
        .send()
        .await
        .expect("post the DigitalOcean token");
    assert_eq!(response.status(), 400);
    let status = wait_for_status(&harness, &attempt).await;
    let AttemptStatus::Failed { message: failed, .. } = status else {
        panic!("the DigitalOcean attempt should fail with {message}, got {status:?}")
    };
    assert_eq!(failed, message);
    assert_eq!(harness.services.credentials.list(&integration_id()).expect("list the DigitalOcean credentials"), []);
    let retry = connect(&harness).await;
    cancel(&harness, &retry).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn digitalocean_plugin_is_registered_alongside_the_other_provider_plugins() {
    assert_eq!(digitalocean::ID, "opencode.provider.digitalocean");
    assert!(opencode_catalog::provider_plugin::ORDER.iter().any(|slot| slot.id() == digitalocean::ID));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registers_browser_oauth_alongside_generic_key_and_environment_methods() {
    let mut harness = start(Input::new(Vec::new())).await;
    let id = integration_id();
    let _seeded = harness
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
                    names: vec!["DIGITALOCEAN_ACCESS_TOKEN".to_owned()],
                }),
            });
            Ok(())
        })
        .await;
    add_plugin(&mut harness, Routes::new(), Options::default()).await;

    let methods = harness
        .services
        .integration
        .get(&integration_id())
        .await
        .expect("get the integration")
        .expect("the DigitalOcean integration")
        .methods;
    assert_eq!(methods.len(), 3);
    assert!(matches!(&methods[0], IntegrationMethod::Key(key) if key.r#type == IntegrationKeyMethodType::Key));
    assert!(matches!(&methods[1], IntegrationMethod::Env(env) if env.names == ["DIGITALOCEAN_ACCESS_TOKEN"]));
    let IntegrationMethod::Oauth(oauth) = &methods[2] else { panic!("the browser OAuth method is last") };
    assert_eq!(oauth.id, method_id());
    assert_eq!(oauth.label, "Login with DigitalOcean");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loads_routers_for_the_signed_in_account() {
    let mut harness = start(Input::new(Vec::new())).await;
    let routes = Routes::new();
    let _seeded = seed(&harness).await;
    create_credential(&harness, oauth_value("do-token", 0.0), None);
    routes.route_json("GET", ROUTERS, routers(&["alpha", "configured"]));
    add_plugin(&mut harness, routes.clone(), Options::default()).await;
    wait_for_model(&harness, "router:alpha").await;

    let requests = routes.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].url, ROUTERS);
    assert_eq!(authorization(&requests[0]).as_deref(), Some("Bearer do-token"));
    assert_eq!(header(&requests[0], "accept").as_deref(), Some("application/json"));
    assert!(header(&requests[0], "user-agent").is_some_and(|value| !value.is_empty()));

    let alpha = harness.services.model.get(&provider_id(), "router:alpha").await.unwrap().expect("router:alpha");
    assert_eq!(alpha.name, "alpha");
    assert_eq!(alpha.family.as_deref(), Some("digitalocean-inference-routers"));
    assert_eq!(alpha.package.as_deref(), Some(OPENAI_COMPATIBLE));
    assert_eq!(model_settings(&alpha), Some(json!({ "baseURL": BASE_URL })));
    assert!(alpha.capabilities.tools);
    assert_eq!(alpha.capabilities.freeform, None);
    assert_eq!(alpha.capabilities.input, ["text"]);
    assert_eq!(alpha.capabilities.output, ["text"]);
    assert_eq!(alpha.capabilities.temperature, None);
    assert_eq!(alpha.limit.context, 128_000.0);
    assert_eq!(alpha.limit.output, 8_192.0);

    let configured =
        harness.services.model.get(&provider_id(), "router:configured").await.unwrap().expect("router:configured");
    assert_eq!(configured.name, "Configured router");
    assert_eq!(configured.limit.context, 256_000.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loads_nothing_and_logs_nothing_at_startup_when_the_access_token_has_expired() {
    let mut harness = start(Input::new(Vec::new())).await;
    let routes = Routes::new();
    let _seeded = seed(&harness).await;
    create_credential(&harness, oauth_value("do-token", 1.0), None);
    add_plugin(&mut harness, routes.clone(), Options::default()).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(routes.requests().is_empty());
    assert!(harness.services.model.get(&provider_id(), "router:alpha").await.unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refreshes_routers_in_the_background_with_the_latest_access_token() {
    let mut harness = start(Input::new(Vec::new())).await;
    let routes = Routes::new();
    let _seeded = seed(&harness).await;
    routes.route_json("GET", ROUTERS, routers(&["alpha"]));
    let credential = create_credential(&harness, oauth_value("do-token", 0.0), None);
    add_plugin(&mut harness, routes.clone(), Options { interval: Some(Duration::from_millis(20)) }).await;
    wait_for_requests(&routes, 1).await;
    assert_eq!(authorization(&routes.requests()[0]).as_deref(), Some("Bearer do-token"));

    harness
        .services
        .credentials
        .update(&credential.id, Update { label: None, value: Some(oauth_value("rotated-token", 0.0)) })
        .expect("update the DigitalOcean credential");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let baseline = routes.requests().len();
    wait_for_requests(&routes, baseline + 1).await;

    for request in &routes.requests()[baseline..] {
        assert_eq!(authorization(request).as_deref(), Some("Bearer rotated-token"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loads_on_credential_changes_and_clears_the_previous_account_routers() {
    let mut harness = start(Input::new(Vec::new())).await;
    let routes = Routes::new();
    let _seeded = seed(&harness).await;
    routes.route_json("GET", ROUTERS, routers(&["alpha", "configured"]));
    add_plugin(&mut harness, routes.clone(), Options::default()).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(routes.requests().is_empty());

    create_credential(&harness, oauth_value("do-token", 0.0), None);
    wait_for_model(&harness, "router:alpha").await;

    routes.set_status("GET", ROUTERS, 401);
    let second = create_credential(&harness, oauth_value("other-account", 0.0), None);
    wait_for_authorization(&routes, "Bearer other-account").await;
    wait_for_model_absent(&harness, "router:alpha").await;
    assert_eq!(
        authorization(routes.requests().last().expect("a discovery request")).as_deref(),
        Some("Bearer other-account")
    );

    tokio::time::sleep(Duration::from_millis(100)).await;
    routes.set_json("GET", ROUTERS, routers(&["beta"]));
    let count = routes.requests().len();
    create_credential(&harness, key_value("do-key"), None);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(routes.requests().len(), count);
    assert!(harness.services.model.get(&provider_id(), "router:beta").await.unwrap().is_none());

    harness.services.credentials.activate(&second.id).expect("activate the second DigitalOcean credential");
    wait_for_model(&harness, "router:beta").await;
    harness.services.credentials.remove(&second.id).expect("remove the second DigitalOcean credential");
    wait_for_model_absent(&harness, "router:beta").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shows_a_returning_accounts_cached_routers_at_once_and_refreshes_them_in_the_background() {
    let mut harness = start(Input::new(Vec::new())).await;
    let routes = Routes::new();
    let _seeded = seed(&harness).await;
    routes.route_json("GET", ROUTERS, routers(&["alpha"]));
    let first = create_credential(&harness, oauth_value("do-token", 0.0), None);
    add_plugin(&mut harness, routes.clone(), Options::default()).await;
    wait_for_model(&harness, "router:alpha").await;

    routes.set_json("GET", ROUTERS, routers(&["beta"]));
    create_credential(&harness, oauth_value("other-account", 0.0), None);
    wait_for_model(&harness, "router:beta").await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    routes.set_json("GET", ROUTERS, routers(&["gamma"]));
    routes.hold("GET", ROUTERS);
    let count = routes.requests().len();
    harness.services.credentials.activate(&first.id).expect("activate the first DigitalOcean credential");
    routes.arrived().await;
    wait_for_model(&harness, "router:alpha").await;
    assert_eq!(routes.requests().len(), count + 1);

    routes.release();
    wait_for_model(&harness, "router:gamma").await;
    wait_for_model_absent(&harness, "router:alpha").await;
    assert_eq!(routes.requests().len(), count + 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completes_browser_oauth_independently_of_router_discovery_and_releases_the_listener() {
    let _guard = PORT.lock().await;
    let mut harness = start(Input::new(Vec::new())).await;
    let routes = Routes::new();
    routes.set_status("GET", ROUTERS, 503);
    let _seeded = seed(&harness).await;
    add_plugin(&mut harness, routes, Options::default()).await;

    let attempt = connect(&harness).await;
    assert_eq!(attempt.mode, AttemptMode::Auto);
    let url = url::Url::parse(&attempt.url).expect("the DigitalOcean authorization url");
    assert_eq!(url.scheme(), "https");
    assert_eq!(url.host_str(), Some("cloud.digitalocean.com"));
    assert_eq!(url.path(), "/v1/oauth/authorize");
    assert_eq!(param(&url, "response_type").as_deref(), Some("token"));
    assert_eq!(
        param(&url, "client_id").as_deref(),
        Some("b1a6c5158156caac821fd1b30253ca8acb52454a48fa744420e41889cb589f82")
    );
    assert_eq!(param(&url, "scope").as_deref(), Some("genai:read inference:query"));
    assert_eq!(param(&url, "redirect_uri").as_deref(), Some("http://localhost:1456/auth/callback"));
    let state = param(&url, "state").expect("the authorization state");
    assert!(!state.is_empty());

    let client = opencode_config::http::client();
    let page = client
        .get("http://localhost:1456/auth/callback")
        .send()
        .await
        .expect("read the DigitalOcean authorization page")
        .text()
        .await
        .expect("the DigitalOcean authorization page body");
    assert!(page.contains("/auth/token"));

    let before = opencode_util::time::now_ms();
    let response = client
        .post("http://localhost:1456/auth/token")
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::json!({ "access_token": "do-token", "expires_in": "3600", "state": state }).to_string())
        .send()
        .await
        .expect("post the DigitalOcean token");
    assert_eq!(response.status(), 200);
    let status = wait_for_status(&harness, &attempt).await;
    let after = opencode_util::time::now_ms();
    assert!(matches!(status, AttemptStatus::Complete { .. }));

    let credentials = harness.services.credentials.list(&integration_id()).expect("list the DigitalOcean credentials");
    assert_eq!(credentials.len(), 1);
    let CredentialValue::Oauth(saved) = &credentials[0].value else {
        panic!("the saved DigitalOcean credential is OAuth")
    };
    assert_eq!(saved.r#type, CredentialOAuthType::Oauth);
    assert_eq!(saved.method_id, method_id());
    assert_eq!(saved.access, "do-token");
    assert_eq!(saved.refresh, "");
    assert_eq!(saved.metadata, None);
    assert!(saved.expires >= before as f64 + 3_600_000.0);
    assert!(saved.expires <= after as f64 + 3_600_000.0);

    let next = connect(&harness).await;
    let next_url = url::Url::parse(&next.url).expect("the next DigitalOcean authorization url");
    assert_ne!(param(&next_url, "state"), Some(state));
    cancel(&harness, &next).await;
    let retry = connect(&harness).await;
    cancel(&harness, &retry).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_mismatched_state_and_releases_the_listener() {
    rejects_callback(&json!({ "access_token": "token", "state": "wrong" }).to_string(), "Invalid OAuth state").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_missing_token_and_releases_the_listener() {
    rejects_callback(&json!({}).to_string(), "Missing access token").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_malformed_json_and_releases_the_listener() {
    rejects_callback("not-json", "Missing access token").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_provider_denial_and_releases_the_listener() {
    rejects_callback(&json!({ "error": "access_denied", "error_description": "Denied" }).to_string(), "Denied").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_provider_denial_without_a_description_and_releases_the_listener() {
    rejects_callback(&json!({ "error": "access_denied", "error_description": "" }).to_string(), "access_denied").await;
}
