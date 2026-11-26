use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use opencode_ai::route::executor::PreparedHttp;
use opencode_catalog::builtin::Builtin;
use opencode_catalog::credential::{CreateInput, Info};
use opencode_catalog::integration::{
    Attempt, AttemptError, AttemptMode, AttemptStatus, AuthorizationError, Implementation, KeyInput, OAuthConnectInput,
};
use opencode_catalog::provider_plugin::gitlab;
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    ConnectionCredentialInfo, ConnectionCredentialInfoMethod, ConnectionCredentialInfoType, ConnectionInfo,
    CredentialOAuth, CredentialOAuthType, CredentialValue, FormAnswer, FormField1, FormValue, IntegrationEnvMethod,
    IntegrationEnvMethodType, IntegrationID, IntegrationKeyMethod, IntegrationKeyMethodType, IntegrationMethod,
    IntegrationMethodID, IntegrationOAuthMethodType,
};
use serde_json::{Value, json};
use url::Url;

use crate::builtin::{Harness, Input, start};
use crate::provider_plugin::support::{Reply, Routes, plugin_context_with};

const INTEGRATION_ID: &str = "gitlab";

const METHOD_ID: &str = "pkce";

const BUNDLED_CLIENT_ID: &str = "fd180700a8f9c5d5557aca231632dd0611a1135a3bb510a741d5a988a3394fa7";

const LEGACY_CLIENT_ID: &str = "1d89f9fdb23ee96d4e603201f6861dab6e143c5c3c00469a018a2d94bdc03d4e";

const GITLAB_COM_AUTHORIZE: &str = "https://gitlab.com/oauth/authorize";

const GITLAB_COM_TOKEN: &str = "https://gitlab.com/oauth/token";

const REDIRECT_URI: &str = "http://127.0.0.1:8080/callback";

static PORT: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));

static REFRESH: AtomicU64 = AtomicU64::new(0);

fn integration_id() -> IntegrationID {
    IntegrationID(INTEGRATION_ID.to_owned())
}

fn method_id() -> IntegrationMethodID {
    IntegrationMethodID(METHOD_ID.to_owned())
}

fn client() -> reqwest::Client {
    opencode_config::http::client()
}

fn renewal() -> Value {
    json!({ "access_token": "renewed-access", "refresh_token": "renewed-refresh", "expires_in": 3600 })
}

fn metadata(instance_url: &str, client_id: Option<&str>) -> JsonMap<Value> {
    let mut entries = vec![("instanceUrl".to_owned(), json!(instance_url))];
    if let Some(client_id) = client_id {
        entries.push(("clientID".to_owned(), json!(client_id)));
    }
    JsonMap(entries)
}

fn expired(prefix: &str, metadata: JsonMap<Value>) -> CredentialValue {
    CredentialValue::Oauth(CredentialOAuth {
        r#type: CredentialOAuthType::Oauth,
        method_id: method_id(),
        access: "stale-access".to_owned(),
        refresh: format!("refresh-{prefix}-{}", REFRESH.fetch_add(1, Ordering::SeqCst)),
        expires: 1.0,
        metadata: Some(metadata),
    })
}

fn answer(instance_url: &str) -> FormAnswer {
    JsonMap(vec![("instanceUrl".to_owned(), FormValue::String(instance_url.to_owned()))])
}

fn connection_of(saved: &Info) -> ConnectionInfo {
    ConnectionInfo::Credential(ConnectionCredentialInfo {
        r#type: ConnectionCredentialInfoType::Credential,
        id: saved.id.clone(),
        label: saved.label.clone(),
        method: ConnectionCredentialInfoMethod::Oauth,
        status: None,
    })
}

fn metadata_json(value: &CredentialOAuth) -> Value {
    value
        .metadata
        .as_ref()
        .map(|metadata| {
            serde_json::from_str(&opencode_contract::codec::encode_json(metadata)).expect("the GitLab metadata json")
        })
        .unwrap_or(Value::Null)
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

fn form(request: &PreparedHttp) -> Vec<(String, String)> {
    url::form_urlencoded::parse(&request.body).map(|(key, value)| (key.into_owned(), value.into_owned())).collect()
}

fn form_param(fields: &[(String, String)], name: &str) -> Option<String> {
    fields.iter().find(|(key, _)| key == name).map(|(_, value)| value.clone())
}

fn header(request: &PreparedHttp, name: &str) -> Option<String> {
    request.headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.clone())
}

fn token_requests(routes: &Routes) -> Vec<PreparedHttp> {
    routes.requests().into_iter().filter(|request| request.url.ends_with("/oauth/token")).collect()
}

fn saved_credentials(harness: &Harness) -> Vec<Info> {
    harness.services.credentials.list(&integration_id()).expect("list the GitLab credentials")
}

async fn active(harness: &Harness) -> Option<ConnectionInfo> {
    harness.services.integration.connection().active(&integration_id()).await.expect("the active GitLab connection")
}

async fn add_plugin(harness: &mut Harness, routes: std::sync::Arc<Routes>) {
    let context = plugin_context_with(harness, routes);
    let mut builtin = Builtin::default();
    gitlab::register_with(&harness.services, &context, &mut builtin, gitlab::Options::default()).await;
    harness.record(builtin);
}

async fn seed_methods(harness: &mut Harness, methods: Vec<IntegrationMethod>) {
    let id = integration_id();
    let registration = harness
        .services
        .integration
        .transform(move |editor| {
            for method in &methods {
                editor.method_update(Implementation::Method { integration_id: id.clone(), method: method.clone() });
            }
            Ok(())
        })
        .await;
    harness.registrations.registrations.push(registration);
}

fn key_method() -> IntegrationMethod {
    IntegrationMethod::Key(IntegrationKeyMethod { r#type: IntegrationKeyMethodType::Key, label: None, form: None })
}

fn create_credential(harness: &Harness, value: CredentialValue) -> Info {
    harness
        .services
        .credentials
        .create(CreateInput { id: None, integration_id: integration_id(), value, label: None, activate: None })
        .expect("create the GitLab credential")
}

async fn try_connect(harness: &Harness, answer: Option<FormAnswer>) -> Result<Attempt, AttemptError> {
    harness
        .services
        .integration
        .oauth()
        .connect(OAuthConnectInput { integration_id: integration_id(), method_id: method_id(), answer, label: None })
        .await
}

async fn connect(harness: &Harness, answer: Option<FormAnswer>) -> Attempt {
    try_connect(harness, answer).await.expect("connect the GitLab integration")
}

struct Login {
    attempt: Attempt,
    url: Url,
    callback: Url,
}

async fn login_with(harness: &Harness, answer: Option<FormAnswer>) -> Login {
    let attempt = connect(harness, answer).await;
    let url = Url::parse(&attempt.url).expect("the GitLab authorization url");
    let redirect = param(&url, "redirect_uri").expect("the GitLab redirect");
    let mut callback = Url::parse(&redirect).expect("the GitLab callback url");
    set_param(&mut callback, "state", &param(&url, "state").expect("the GitLab state"));
    set_param(&mut callback, "code", "auth-code");
    Login { attempt, url, callback }
}

async fn login(harness: &Harness) -> Login {
    login_with(harness, None).await
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
                .expect("the GitLab attempt status");
            if !matches!(status, AttemptStatus::Pending { .. }) {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the GitLab attempt leaves pending")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gitlab_plugin_is_registered_alongside_the_other_provider_plugins() {
    assert_eq!(gitlab::ID, "opencode.provider.gitlab");
    assert!(opencode_catalog::provider_plugin::ORDER.iter().any(|slot| slot.id() == gitlab::ID));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registers_pkce_oauth_alongside_the_generic_key_and_env_methods() {
    let routes = Routes::new();
    let mut harness = start(Input::new(Vec::new())).await;
    seed_methods(
        &mut harness,
        vec![
            key_method(),
            IntegrationMethod::Env(IntegrationEnvMethod {
                r#type: IntegrationEnvMethodType::Env,
                names: vec!["GITLAB_TOKEN".to_owned()],
            }),
        ],
    )
    .await;
    add_plugin(&mut harness, routes.clone()).await;

    let methods = harness
        .services
        .integration
        .get(&integration_id())
        .await
        .expect("get the GitLab integration")
        .expect("the GitLab integration")
        .methods;
    assert_eq!(methods.len(), 3);
    assert!(matches!(&methods[0], IntegrationMethod::Key(key) if key.r#type == IntegrationKeyMethodType::Key));
    assert!(matches!(&methods[1], IntegrationMethod::Env(env) if env.names == ["GITLAB_TOKEN"]));
    let IntegrationMethod::Oauth(oauth) = &methods[2] else { panic!("the GitLab OAuth method is last") };
    assert_eq!(oauth.id, method_id());
    assert_eq!(oauth.r#type, IntegrationOAuthMethodType::Oauth);
    assert_eq!(oauth.label, "Login with GitLab (OAuth)");
    let form = oauth.form.as_ref().expect("the GitLab OAuth form");
    assert_eq!(form.0.len(), 1);
    let FormField1::String(field) = &form.0[0] else { panic!("the GitLab instance URL field is a string") };
    assert_eq!(field.key, "instanceUrl");
    assert_eq!(field.title.as_deref(), Some("GitLab instance URL"));
    assert_eq!(
        field.description.as_deref(),
        Some("Leave the default to use gitlab.com, or enter your self-managed GitLab URL.")
    );
    assert_eq!(field.placeholder.as_deref(), Some("https://gitlab.com"));
    assert_eq!(field.default.as_deref(), Some("https://gitlab.com"));
    assert_eq!(field.pattern.as_deref(), Some("^https?://\\S+$"));
    assert!(routes.requests().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exchanges_a_pkce_code_for_a_native_gitlab_oauth_credential_against_gitlab_com_by_default() {
    let _guard = PORT.lock().await;
    let routes = Routes::new();
    let mut harness = start(Input::new(Vec::new())).await;
    add_plugin(&mut harness, routes.clone()).await;
    let login = login(&harness).await;
    assert_eq!(login.attempt.mode, AttemptMode::Auto);
    assert_eq!(format!("{}{}", login.url.origin().ascii_serialization(), login.url.path()), GITLAB_COM_AUTHORIZE);
    assert_eq!(param(&login.url, "response_type").as_deref(), Some("code"));
    assert_eq!(param(&login.url, "client_id").as_deref(), Some(BUNDLED_CLIENT_ID));
    assert_eq!(param(&login.url, "scope").as_deref(), Some("api"));
    assert_eq!(param(&login.url, "code_challenge_method").as_deref(), Some("S256"));
    assert_eq!(param(&login.url, "redirect_uri").as_deref(), Some(REDIRECT_URI));
    assert_eq!(login.callback.host_str(), Some("127.0.0.1"));
    assert_eq!(login.callback.port(), Some(8080));
    assert_eq!(login.callback.path(), "/callback");

    routes.route_json(
        "POST",
        GITLAB_COM_TOKEN,
        json!({ "access_token": "access-token", "refresh_token": "refresh-token", "expires_in": 7200 }),
    );
    let before = opencode_util::time::now_ms();
    let response = client().get(login.callback.as_str()).send().await.expect("fetch the GitLab callback");
    assert_eq!(response.status(), 200);
    assert!(response.text().await.expect("the GitLab callback page").contains("Authorization successful"));
    let status = wait_for_status(&harness, &login.attempt).await;
    let after = opencode_util::time::now_ms();
    assert!(matches!(status, AttemptStatus::Complete { .. }), "got {status:?}");

    let requests = token_requests(&routes);
    assert_eq!(requests.len(), 1);
    let exchange = &requests[0];
    assert_eq!(exchange.url, GITLAB_COM_TOKEN);
    assert!(header(exchange, "content-type").is_some_and(|value| value.contains("application/x-www-form-urlencoded")));
    let fields = form(exchange);
    assert_eq!(form_param(&fields, "grant_type").as_deref(), Some("authorization_code"));
    assert_eq!(form_param(&fields, "client_id").as_deref(), Some(BUNDLED_CLIENT_ID));
    assert_eq!(form_param(&fields, "code").as_deref(), Some("auth-code"));
    assert_eq!(form_param(&fields, "redirect_uri").as_deref(), Some(REDIRECT_URI));
    let verifier = form_param(&fields, "code_verifier").expect("the GitLab code verifier");
    assert_eq!(
        param(&login.url, "code_challenge").as_deref(),
        Some(opencode_catalog::oauth::pkce::challenge(&verifier).as_str())
    );

    let saved = saved_credentials(&harness);
    assert_eq!(saved.len(), 1);
    let CredentialValue::Oauth(value) = &saved[0].value else { panic!("the GitLab credential is OAuth") };
    assert_eq!(value.r#type, CredentialOAuthType::Oauth);
    assert_eq!(value.method_id, method_id());
    assert_eq!(value.access, "access-token");
    assert_eq!(value.refresh, "refresh-token");
    assert_eq!(metadata_json(value), json!({ "instanceUrl": "https://gitlab.com", "clientID": BUNDLED_CLIENT_ID }));
    assert!(value.expires >= before as f64 + 7_200_000.0);
    assert!(value.expires <= after as f64 + 7_200_000.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uses_the_answered_self_managed_instance_url_including_a_relative_url_root() {
    let _guard = PORT.lock().await;
    let routes = Routes::new();
    let mut harness = start(Input::new(Vec::new()).environment("GITLAB_OAUTH_CLIENT_ID", "self-managed-client")).await;
    add_plugin(&mut harness, routes.clone()).await;
    let login = login_with(&harness, Some(answer("https://example.com/gitlab/"))).await;
    assert_eq!(
        format!("{}{}", login.url.origin().ascii_serialization(), login.url.path()),
        "https://example.com/gitlab/oauth/authorize"
    );

    routes.route_json(
        "POST",
        "https://example.com/gitlab/oauth/token",
        json!({ "access_token": "access-token", "refresh_token": "refresh-token", "expires_in": 3600 }),
    );
    let response = client().get(login.callback.as_str()).send().await.expect("fetch the GitLab callback");
    assert_eq!(response.status(), 200);
    let status = wait_for_status(&harness, &login.attempt).await;
    assert!(matches!(status, AttemptStatus::Complete { .. }), "got {status:?}");

    let requests = token_requests(&routes);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, "https://example.com/gitlab/oauth/token");

    let saved = saved_credentials(&harness);
    assert_eq!(saved.len(), 1);
    let CredentialValue::Oauth(value) = &saved[0].value else { panic!("the GitLab credential is OAuth") };
    assert_eq!(
        metadata_json(value),
        json!({ "instanceUrl": "https://example.com/gitlab", "clientID": "self-managed-client" })
    );
    assert_eq!(saved[0].label, "example.com");
    let connection = active(&harness).await.expect("the GitLab connection");
    assert!(matches!(&connection, ConnectionInfo::Credential(connection) if connection.label == "example.com"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fails_early_for_self_managed_instances_without_gitlab_oauth_client_id() {
    let routes = Routes::new();
    let mut harness = start(Input::new(Vec::new())).await;
    add_plugin(&mut harness, routes.clone()).await;
    let error = try_connect(&harness, Some(answer("https://gitlab.example.com")))
        .await
        .expect_err("the self-managed GitLab login fails without a client ID");
    let AttemptError::Authorization(error) = error else { panic!("the GitLab login fails before binding") };
    let message = error.message();
    assert!(message.contains("only exists on gitlab.com"));
    assert!(message.contains("GITLAB_OAUTH_CLIENT_ID"));
    assert!(message.contains("gitlab.example.com"));
    assert!(routes.requests().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn treats_gitlab_com_on_a_non_default_port_as_self_managed() {
    let routes = Routes::new();
    let mut harness = start(Input::new(Vec::new())).await;
    add_plugin(&mut harness, routes.clone()).await;
    let error = try_connect(&harness, Some(answer("https://gitlab.com:8443")))
        .await
        .expect_err("gitlab.com on another port needs its own client ID");
    let AttemptError::Authorization(error) = error else { panic!("the GitLab login fails before binding") };
    assert!(error.message().contains("To sign in to gitlab.com:8443,"));
    assert!(routes.requests().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refreshes_an_oauth_credential_using_its_stored_instance_url_and_includes_redirect_uri() {
    let routes = Routes::new();
    let mut harness = start(Input::new(Vec::new())).await;
    add_plugin(&mut harness, routes.clone()).await;
    let now = opencode_util::time::now_ms();
    routes.route_json("POST", "https://gitlab.example.com/oauth/token", renewal());
    let saved =
        create_credential(&harness, expired("stored", metadata("https://gitlab.example.com", Some(BUNDLED_CLIENT_ID))));
    let CredentialValue::Oauth(stored) = &saved.value else { panic!("the GitLab credential is OAuth") };
    let refresh = stored.refresh.clone();

    let resolved = harness
        .services
        .integration
        .connection()
        .resolve(&connection_of(&saved))
        .await
        .expect("resolve the GitLab credential")
        .expect("the GitLab credential");
    let CredentialValue::Oauth(value) = resolved else { panic!("the GitLab credential is OAuth") };
    assert_eq!(value.access, "renewed-access");
    assert_eq!(value.refresh, "renewed-refresh");
    assert!(value.expires >= now as f64 + 3_600_000.0);
    assert_eq!(
        metadata_json(&value),
        json!({ "instanceUrl": "https://gitlab.example.com", "clientID": BUNDLED_CLIENT_ID })
    );

    let requests = token_requests(&routes);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, "https://gitlab.example.com/oauth/token");
    let fields = form(&requests[0]);
    assert_eq!(form_param(&fields, "grant_type").as_deref(), Some("refresh_token"));
    assert_eq!(form_param(&fields, "refresh_token").as_deref(), Some(refresh.as_str()));
    assert_eq!(form_param(&fields, "client_id").as_deref(), Some(BUNDLED_CLIENT_ID));
    assert_eq!(form_param(&fields, "redirect_uri").as_deref(), Some(REDIRECT_URI));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shares_one_token_exchange_between_concurrent_refreshes_of_the_same_credential() {
    let routes = Routes::new();
    let mut harness = start(Input::new(Vec::new())).await;
    add_plugin(&mut harness, routes.clone()).await;
    routes.route_json("POST", GITLAB_COM_TOKEN, renewal());
    routes.hold("POST", GITLAB_COM_TOKEN);
    let saved = create_credential(&harness, expired("shared", metadata("https://gitlab.com", Some(BUNDLED_CLIENT_ID))));
    let connection = connection_of(&saved);
    let services = harness.services.integration.clone();

    let (first, second, third, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            async { services.connection().resolve(&connection).await },
            async { services.connection().resolve(&connection).await },
            async { services.connection().resolve(&connection).await },
            async {
                routes.arrived().await;
                routes.release();
            },
        )
    })
    .await
    .expect("the shared GitLab refresh settles");

    let access = |resolved: Result<Option<CredentialValue>, _>| match resolved.expect("resolve the GitLab credential") {
        Some(CredentialValue::Oauth(value)) => value.access,
        _ => panic!("the GitLab credential is OAuth"),
    };
    assert_eq!([access(first), access(second), access(third)], ["renewed-access", "renewed-access", "renewed-access"]);
    assert_eq!(token_requests(&routes).len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refreshes_credentials_without_a_client_id_with_the_opencode_gitlab_auth_application() {
    let routes = Routes::new();
    let mut harness = start(Input::new(Vec::new())).await;
    add_plugin(&mut harness, routes.clone()).await;
    routes.route_json("POST", GITLAB_COM_TOKEN, renewal());
    let saved = create_credential(&harness, expired("legacy", metadata("https://gitlab.com", None)));

    let resolved = harness
        .services
        .integration
        .connection()
        .resolve(&connection_of(&saved))
        .await
        .expect("resolve the GitLab credential")
        .expect("the GitLab credential");
    let CredentialValue::Oauth(value) = resolved else { panic!("the GitLab credential is OAuth") };
    assert_eq!(value.access, "renewed-access");
    assert_eq!(metadata_json(&value), json!({ "instanceUrl": "https://gitlab.com", "clientID": LEGACY_CLIENT_ID }));

    let clients: Vec<Option<String>> =
        token_requests(&routes).iter().map(|request| form_param(&form(request), "client_id")).collect();
    assert_eq!(clients, [Some(LEGACY_CLIENT_ID.to_owned())]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refreshes_only_with_the_recorded_client_id_when_the_credential_has_one() {
    let routes = Routes::new();
    let mut harness = start(Input::new(Vec::new())).await;
    add_plugin(&mut harness, routes.clone()).await;
    routes.route_json("POST", GITLAB_COM_TOKEN, renewal());
    let saved =
        create_credential(&harness, expired("recorded", metadata("https://gitlab.com", Some(LEGACY_CLIENT_ID))));

    harness
        .services
        .integration
        .connection()
        .resolve(&connection_of(&saved))
        .await
        .expect("resolve the GitLab credential");

    let clients: Vec<Option<String>> =
        token_requests(&routes).iter().map(|request| form_param(&form(request), "client_id")).collect();
    assert_eq!(clients, [Some(LEGACY_CLIENT_ID.to_owned())]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_a_revoked_refresh_token_with_a_sign_in_again_hint_instead_of_authorization_code_hints() {
    let routes = Routes::new();
    let mut harness = start(Input::new(Vec::new())).await;
    add_plugin(&mut harness, routes.clone()).await;
    routes.route_with("POST", GITLAB_COM_TOKEN, |_| Reply::json_status(json!({ "error": "invalid_grant" }), 400));
    let saved =
        create_credential(&harness, expired("revoked", metadata("https://gitlab.com", Some(BUNDLED_CLIENT_ID))));

    let error = harness
        .services
        .integration
        .connection()
        .resolve(&connection_of(&saved))
        .await
        .expect_err("the revoked GitLab refresh token is rejected");
    let message = error.message();
    assert!(message.contains("refresh token was revoked, expired"));
    assert!(message.contains("Sign in to GitLab again"));
    assert!(!message.contains("authorization code"));
}

async fn rejects_callback(params: &[(&str, &str)], message: &str) {
    let _guard = PORT.lock().await;
    let routes = Routes::new();
    let mut harness = start(Input::new(Vec::new())).await;
    add_plugin(&mut harness, routes.clone()).await;
    let login = login(&harness).await;
    let mut callback = login.callback.clone();
    for (name, value) in params {
        set_param(&mut callback, name, value);
    }

    let response = client().get(callback.as_str()).send().await.expect("fetch the GitLab callback");
    assert_eq!(response.status(), 400);
    assert!(response.text().await.expect("the GitLab callback page").contains("Authorization failed"));
    let status = wait_for_status(&harness, &login.attempt).await;
    let AttemptStatus::Failed { message: failed, .. } = status else {
        panic!("the GitLab attempt fails with {message}, got {status:?}")
    };
    assert_eq!(failed, message);
    assert!(routes.requests().is_empty());
    assert_eq!(saved_credentials(&harness), []);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ignores_a_callback_with_the_wrong_state_and_still_completes_the_login() {
    let _guard = PORT.lock().await;
    let routes = Routes::new();
    let mut harness = start(Input::new(Vec::new())).await;
    add_plugin(&mut harness, routes.clone()).await;
    let login = login(&harness).await;

    let mut stray = login.callback.clone();
    set_param(&mut stray, "state", "another-login");
    let response = client().get(stray.as_str()).send().await.expect("fetch the GitLab callback");
    assert_eq!(response.status(), 400);
    assert!(response.text().await.expect("the GitLab callback page").contains("Authorization failed"));

    routes.route_json(
        "POST",
        GITLAB_COM_TOKEN,
        json!({ "access_token": "access-token", "refresh_token": "refresh-token", "expires_in": 7200 }),
    );
    let response = client().get(login.callback.as_str()).send().await.expect("fetch the GitLab callback");
    assert_eq!(response.status(), 200);
    let status = wait_for_status(&harness, &login.attempt).await;
    assert!(matches!(status, AttemptStatus::Complete { .. }), "got {status:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_invalid_or_denied_callbacks_without_a_code() {
    rejects_callback(&[("code", "")], "Missing authorization code").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_invalid_or_denied_callbacks_with_a_denied_description() {
    rejects_callback(
        &[("error", "access_denied"), ("error_description", "User declined access")],
        "User declined access",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_invalid_or_denied_callbacks_with_a_denied_error() {
    rejects_callback(&[("error", "access_denied")], "access_denied").await;
}

async fn preserves_connection(status: u16, body: &str, contains: &str) {
    let _guard = PORT.lock().await;
    let routes = Routes::new();
    let mut harness = start(Input::new(Vec::new())).await;
    seed_methods(&mut harness, vec![key_method()]).await;
    add_plugin(&mut harness, routes.clone()).await;
    harness
        .services
        .integration
        .connection()
        .key(KeyInput { integration_id: integration_id(), key: "previous-key".to_owned(), answer: None, label: None })
        .await
        .expect("create the previous GitLab key");
    let previous = active(&harness).await.expect("the previous GitLab connection");
    let saved = saved_credentials(&harness);
    let login = login(&harness).await;
    let body = body.to_owned();
    routes.route_with("POST", GITLAB_COM_TOKEN, move |_| Reply { status, body: body.as_bytes().to_vec() });

    let page = client().get(login.callback.as_str()).send().await.expect("fetch the GitLab callback");
    assert_eq!(page.status(), 400);
    assert!(page.text().await.expect("the GitLab callback page").contains(contains));
    let status = wait_for_status(&harness, &login.attempt).await;
    assert!(matches!(status, AttemptStatus::Failed { .. }), "got {status:?}");
    assert_eq!(saved_credentials(&harness), saved);
    assert_eq!(active(&harness).await, Some(previous));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preserves_the_active_connection_after_a_failed_token_exchange_with_a_code_expired_description() {
    preserves_connection(400, r#"{"error":"invalid_grant","error_description":"Code expired"}"#, "Code expired").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preserves_the_active_connection_after_a_failed_token_exchange_with_invalid_grant() {
    preserves_connection(400, r#"{"error":"invalid_grant"}"#, "client_id used:").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preserves_the_active_connection_after_a_failed_token_exchange_with_a_bad_gateway() {
    preserves_connection(502, "Bad gateway", "HTTP 502").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_eaddrinuse_with_a_clear_message_when_the_callback_port_is_occupied() {
    let _guard = PORT.lock().await;
    let routes = Routes::new();
    let mut harness = start(Input::new(Vec::new())).await;
    add_plugin(&mut harness, routes.clone()).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await.expect("occupy the GitLab callback port");

    let error = try_connect(&harness, None).await.expect_err("the occupied port fails the GitLab login");
    let AttemptError::Authorization(error) = error else { panic!("the GitLab login fails at the callback port") };
    assert!(error.message().contains("port 8080"));
    assert!(error.message().contains("already in use"));
    drop(listener);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_abandoned_refresh_leader_does_not_block_later_refreshes() {
    let routes = Routes::new();
    let mut harness = start(Input::new(Vec::new())).await;
    let saved = create_credential(
        &harness,
        expired("abandoned", metadata("https://gitlab.example.com", Some(BUNDLED_CLIENT_ID))),
    );
    let CredentialValue::Oauth(stored) = &saved.value else { panic!("the GitLab credential is OAuth") };
    let key =
        opencode_catalog::provider_plugin::gitlab::refresh_gate::key("https://gitlab.example.com", &stored.refresh);

    let started = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
    let leader = tokio::spawn({
        let key = key.clone();
        let started = started.clone();
        async move {
            opencode_catalog::provider_plugin::gitlab::refresh_gate::run(key, async move {
                started.add_permits(1);
                std::future::pending::<()>().await;
                unreachable!()
            })
            .await
        }
    });
    let _started = started.acquire().await.expect("the abandoned refresh leader starts");

    let waiter = tokio::spawn({
        let key = key.clone();
        async move {
            opencode_catalog::provider_plugin::gitlab::refresh_gate::run(key, async {
                Err(AuthorizationError::new(std::io::Error::other("the abandoned leader was replaced")))
            })
            .await
        }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!waiter.is_finished(), "the second refresh waits on the abandoned leader");

    leader.abort();
    assert!(leader.await.is_err(), "the abandoned refresh leader is cancelled");
    let outcome = tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .expect("the waiting refresh settles once its leader is abandoned")
        .expect("the waiting refresh task");
    let error = outcome.expect_err("an abandoned refresh fails its waiters");
    assert!(error.message().contains("cancelled"), "got {}", error.message());

    routes.route_json("POST", "https://gitlab.example.com/oauth/token", renewal());
    add_plugin(&mut harness, routes.clone()).await;
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let current = harness.services.credentials.get(&saved.id).expect("get the GitLab credential");
            if matches!(current.as_ref().map(|current| &current.value), Some(CredentialValue::Oauth(value)) if value.access == "renewed-access") {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("the GitLab credential refreshes after an abandoned leader");

    let requests = token_requests(&routes);
    assert_eq!(requests.len(), 1);
    let fields = form(&requests[0]);
    assert_eq!(form_param(&fields, "refresh_token").as_deref(), Some(stored.refresh.as_str()));
    assert_eq!(form_param(&fields, "client_id").as_deref(), Some(BUNDLED_CLIENT_ID));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn honors_gitlab_oauth_client_id_for_self_managed_applications() {
    let _guard = PORT.lock().await;
    let routes = Routes::new();
    let mut harness = start(Input::new(Vec::new()).environment("GITLAB_OAUTH_CLIENT_ID", "custom-client-id")).await;
    add_plugin(&mut harness, routes.clone()).await;
    let login = login(&harness).await;
    assert_eq!(param(&login.url, "client_id").as_deref(), Some("custom-client-id"));

    routes.route_json(
        "POST",
        GITLAB_COM_TOKEN,
        json!({ "access_token": "a", "refresh_token": "r", "expires_in": 3600 }),
    );
    let response = client().get(login.callback.as_str()).send().await.expect("fetch the GitLab callback");
    assert_eq!(response.status(), 200);
    let status = wait_for_status(&harness, &login.attempt).await;
    assert!(matches!(status, AttemptStatus::Complete { .. }), "got {status:?}");

    let requests = token_requests(&routes);
    assert_eq!(requests.len(), 1);
    assert_eq!(form_param(&form(&requests[0]), "client_id").as_deref(), Some("custom-client-id"));
}
