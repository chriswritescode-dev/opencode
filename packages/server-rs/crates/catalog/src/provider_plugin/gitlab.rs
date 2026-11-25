use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use opencode_ai::HttpSender;
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    ConnectionInfo, CredentialOAuth, CredentialOAuthType, CredentialValue, FormAnswer, FormField1, FormFields1,
    FormStringField1, FormStringField1Type, FormValue, IntegrationID, IntegrationMethodID, IntegrationOAuthMethod,
    IntegrationOAuthMethodType, ModelCapabilities, ModelInfo, ModelInfoLimit, ProviderID,
};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::oneshot;

use crate::builtin::{Builtin, Services};
use crate::integration::{
    AuthorizationError, AutoCallback, ConfigEnv, Implementation, LabelCallback, OAuthAuthorization, OAuthFuture,
    OAuthImplementation, OAuthMode, RefreshCallback, SubmitCallback,
};
use crate::oauth::callback::{CallbackServer, HandlerFuture, Reply, Request};
use crate::oauth::page::{self, CallbackPageOptions};
use crate::oauth::pkce;
use crate::oauth::redirect::{self, Pasted};
use crate::provider;
use crate::provider_plugin::http::{self, HttpError};
use crate::provider_plugin::{BoxFuture, PluginContext, env_value, refresh, setting, settings};

pub const ID: &str = "opencode.provider.gitlab";

const PROVIDER_ID: &str = "gitlab";

const METHOD_ID: &str = "pkce";

const BUNDLED_CLIENT_ID: &str = "fd180700a8f9c5d5557aca231632dd0611a1135a3bb510a741d5a988a3394fa7";

const LEGACY_CLIENT_ID: &str = "1d89f9fdb23ee96d4e603201f6861dab6e143c5c3c00469a018a2d94bdc03d4e";

const GITLAB_COM_HOST: &str = "gitlab.com";

const OAUTH_SCOPE: &str = "api";

const CALLBACK_HOST: &str = "127.0.0.1";

const CALLBACK_PORT: u16 = 8080;

const CALLBACK_PATH: &str = "/callback";

const REDIRECT_URI: &str = "http://127.0.0.1:8080/callback";

const DEFAULT_INSTANCE_URL: &str = "https://gitlab.com";

const DEFAULT_EXPIRES_SECONDS: f64 = 7200.0;

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);

const REFRESH_RETENTION_MS: f64 = 60_000.0;

const GITLAB_PACKAGE: &str = "aisdk:gitlab-ai-provider";

const INSTRUCTIONS: &str = "Complete authorization in your browser. This window will close automatically.";

const PORT_IN_USE: &str =
    "GitLab login needs local port 8080, but it is already in use. Stop the process using that port and try again.";

const REQUEST_FAILED: &str = "GitLab token exchange request failed";

const INVALID_RESPONSE: &str = "Invalid GitLab token response";

const INVALID_STATE: &str = "Invalid OAuth state";

const AUTHORIZATION_DENIED: &str = "Authorization denied";

const MISSING_CODE: &str = "Missing authorization code";

const LABEL: &str = "Login with GitLab (OAuth)";

#[derive(Clone, Debug, PartialEq)]
pub struct WorkflowModel {
    pub id: String,
    pub r#ref: String,
    pub name: String,
    pub context: f64,
    pub output: f64,
    pub pinned: bool,
}

pub trait Discovery: Send + Sync {
    fn discover(
        &self,
        instance_url: String,
        headers: Vec<(String, String)>,
        directory: String,
        cache_key: String,
    ) -> BoxFuture<Result<Option<Vec<WorkflowModel>>, String>>;
}

pub struct NoDiscovery;

impl Discovery for NoDiscovery {
    fn discover(
        &self,
        _instance_url: String,
        _headers: Vec<(String, String)>,
        _directory: String,
        _cache_key: String,
    ) -> BoxFuture<Result<Option<Vec<WorkflowModel>>, String>> {
        Box::pin(async { Err("GitLab workflow discovery is not available".to_owned()) })
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    pub timeout: Option<Duration>,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Loaded {
    models: Option<Vec<WorkflowModel>>,
    connection: Option<ConnectionInfo>,
}

struct GitlabOAuth {
    integration_id: IntegrationID,
    method: IntegrationOAuthMethod,
    http: Arc<dyn HttpSender>,
    env: ConfigEnv,
    default_instance_url: String,
}

impl OAuthImplementation for GitlabOAuth {
    fn integration_id(&self) -> &IntegrationID {
        &self.integration_id
    }

    fn method(&self) -> &IntegrationOAuthMethod {
        &self.method
    }

    fn authorize(&self, answer: FormAnswer) -> OAuthFuture<OAuthAuthorization> {
        Box::pin(begin_authorization(self.http.clone(), self.env.clone(), self.default_instance_url.clone(), answer))
    }

    fn label(&self) -> Option<LabelCallback> {
        let env = self.env.clone();
        let default_instance_url = self.default_instance_url.clone();
        Some(Box::new(move |value| {
            let instance_url = credential_instance_url(value.metadata.as_ref(), &env, &default_instance_url);
            url::Url::parse(&instance_url).ok().map(|url| url_host(&url))
        }))
    }

    fn refresh(&self) -> Option<RefreshCallback> {
        let http = self.http.clone();
        let env = self.env.clone();
        let default_instance_url = self.default_instance_url.clone();
        Some(Box::new(move |value| {
            Box::pin(refresh_credential(http.clone(), env.clone(), default_instance_url.clone(), value))
        }))
    }
}

struct CallbackMessage {
    code: String,
    respond: Option<oneshot::Sender<Reply>>,
}

type CallbackSender = oneshot::Sender<Result<CallbackMessage, String>>;

type CallbackResponses = Mutex<Option<CallbackSender>>;

pub async fn register(services: &Services, context: &PluginContext, builtin: &mut Builtin) {
    register_with(services, context, builtin, Options::default()).await
}

pub async fn register_with(services: &Services, context: &PluginContext, builtin: &mut Builtin, options: Options) {
    let provider_id = ProviderID(PROVIDER_ID.to_owned());
    let integration_id = IntegrationID(PROVIDER_ID.to_owned());
    let default_instance_url = resolve_default_instance_url(&context.env, DEFAULT_INSTANCE_URL);
    let timeout = options.timeout.unwrap_or(DISCOVERY_TIMEOUT);

    let integration_registration = {
        let integration_id = integration_id.clone();
        let http = context.http.clone();
        let env = context.env.clone();
        let method_default = default_instance_url.clone();
        services
            .integration
            .transform(move |editor| {
                editor.method_update(Implementation::OAuth(Arc::new(GitlabOAuth {
                    integration_id: integration_id.clone(),
                    method: IntegrationOAuthMethod {
                        id: IntegrationMethodID(METHOD_ID.to_owned()),
                        r#type: IntegrationOAuthMethodType::Oauth,
                        label: LABEL.to_owned(),
                        form: Some(FormFields1(vec![FormField1::String(FormStringField1 {
                            key: "instanceUrl".to_owned(),
                            title: Some("GitLab instance URL".to_owned()),
                            description: Some(
                                "Leave the default to use gitlab.com, or enter your self-managed GitLab URL."
                                    .to_owned(),
                            ),
                            required: None,
                            hidden: None,
                            when: None,
                            r#type: FormStringField1Type::String,
                            format: None,
                            min_length: None,
                            max_length: None,
                            pattern: Some("^https?://\\S+$".to_owned()),
                            placeholder: Some(method_default.clone()),
                            default: Some(method_default.clone()),
                            options: None,
                            custom: None,
                        })])),
                    },
                    http: http.clone(),
                    env: env.clone(),
                    default_instance_url: method_default.clone(),
                })));
                Ok(())
            })
            .await
    };
    builtin.registrations.push(integration_registration);

    let login = settings::credential_settings(services, builtin, &integration_id, |value| match value {
        CredentialValue::Oauth(oauth) => settings::metadata_settings(oauth.metadata.as_ref(), &["instanceUrl"]),
        _ => provider::decode_object(serde_json::Map::new()),
    })
    .await;

    let loaded: Arc<Mutex<Loaded>> = Arc::new(Mutex::new(Loaded::default()));

    let load: Arc<dyn Fn() -> BoxFuture<bool> + Send + Sync> = {
        let integrations = services.integration.clone();
        let providers = services.provider.clone();
        let discovery = context.gitlab_discovery.clone();
        let directory = context.location.directory.0.clone();
        let provider_id = provider_id.clone();
        let integration_id = integration_id.clone();
        let default_instance_url = default_instance_url.clone();
        let state = loaded.clone();
        Arc::new(move || {
            let integrations = integrations.clone();
            let providers = providers.clone();
            let discovery = discovery.clone();
            let directory = directory.clone();
            let provider_id = provider_id.clone();
            let integration_id = integration_id.clone();
            let default_instance_url = default_instance_url.clone();
            let state = state.clone();
            Box::pin(async move {
                let (connection, credential) = settings::active_credential(&integrations, &integration_id).await;
                let Some(ConnectionInfo::Credential(stored)) = connection else {
                    return clear(&state);
                };
                let connection = ConnectionInfo::Credential(stored.clone());
                let Some(credential) =
                    credential.filter(|credential| !matches!(credential, CredentialValue::External(_)))
                else {
                    return clear(&state);
                };
                let provider = providers.get(&provider_id).await.ok().flatten();
                let instance_url = match &credential {
                    CredentialValue::Oauth(oauth) => metadata_instance_url(oauth.metadata.as_ref()),
                    _ => None,
                }
                .or_else(|| {
                    setting(provider.as_ref().and_then(|provider| provider.settings.as_ref()), "instanceUrl")
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| default_instance_url.clone());
                let headers = match &credential {
                    CredentialValue::Oauth(oauth) => {
                        vec![("Authorization".to_owned(), format!("Bearer {}", oauth.access))]
                    }
                    CredentialValue::Key(key) => vec![("PRIVATE-TOKEN".to_owned(), key.key.clone())],
                    CredentialValue::External(_) => Vec::new(),
                };
                let remote = match tokio::time::timeout(
                    timeout,
                    discovery.discover(instance_url, headers, directory.clone(), stored.id.0.clone()),
                )
                .await
                {
                    Ok(Ok(remote)) => remote,
                    Ok(Err(message)) => {
                        log::warn!("failed to discover GitLab workflow models: {message}");
                        None
                    }
                    Err(_) => {
                        log::warn!("failed to discover GitLab workflow models: the discovery timed out");
                        None
                    }
                };
                let active = integrations.connection().active(&integration_id).await.ok().flatten();
                if active.as_ref().map(crate::integration::key) != Some(crate::integration::key(&connection)) {
                    return false;
                }
                let next = Loaded { models: remote, connection: Some(connection) };
                let mut current = state.lock().expect("lock the GitLab discovery state");
                let changed = *current != next;
                *current = next;
                changed
            })
        })
    };

    let loading = Arc::new(tokio::sync::Semaphore::new(1));
    let refresh: Arc<dyn Fn() -> BoxFuture<()> + Send + Sync> = {
        let load = load.clone();
        let providers = services.provider.clone();
        let loading = loading.clone();
        Arc::new(move || {
            let load = load.clone();
            let providers = providers.clone();
            let loading = loading.clone();
            Box::pin(async move {
                let _permit = loading.acquire().await;
                refresh::reload_if_changed(load(), providers.reload()).await;
            })
        })
    };
    builtin.tasks.push(refresh::on_switch(&services.bus, PROVIDER_ID, {
        let refresh = refresh.clone();
        move || refresh()
    }));
    builtin.tasks.push(tokio::spawn(refresh()));

    let provider_registration = services
        .provider
        .transform({
            let provider_id = provider_id.clone();
            let login = login.clone();
            let loaded = loaded.clone();
            move |editor| {
                let login_settings = login.lock().expect("lock the GitLab login settings").clone();
                if setting(Some(&login_settings), "instanceUrl").is_some() && editor.get(&provider_id).is_some() {
                    editor.update(&provider_id, |provider| {
                        let base = provider.settings.as_ref().map(provider::json_object);
                        let overlay = provider::json_object(&login_settings);
                        provider.settings = crate::aisdk_native::merge_overlay(base.as_ref(), Some(&overlay))
                            .map(provider::decode_object);
                    });
                }
                let Some(item) = editor.get(&provider_id).cloned() else {
                    return Ok(());
                };
                let Loaded { models, connection } = loaded.lock().expect("lock the GitLab discovery state").clone();
                let Some(models) = models.filter(|models| !models.is_empty()) else {
                    return Ok(());
                };
                let mut discovered: Vec<ModelInfo> = item.models.values().cloned().collect();
                for model in models {
                    if item.models.contains_key(&model.id) {
                        continue;
                    }
                    let mut info = crate::provider::default_model(&provider_id, &model.id);
                    info.name = format!("Agent Platform ({})", model.name);
                    info.package = Some(GITLAB_PACKAGE.to_owned());
                    info.settings = Some(provider::decode_object(serde_json::Map::from_iter([(
                        "workflowRef".to_owned(),
                        Value::String(model.r#ref),
                    )])));
                    info.capabilities = ModelCapabilities {
                        tools: true,
                        freeform: None,
                        input: vec!["text".to_owned(), "image".to_owned(), "pdf".to_owned()],
                        output: vec!["text".to_owned()],
                        temperature: None,
                    };
                    info.limit = ModelInfoLimit { context: model.context, input: None, output: model.output };
                    discovered.push(info);
                }
                editor.add(item.provider, &discovered, connection);
                Ok(())
            }
        })
        .await;
    builtin.registrations.push(provider_registration);
}

fn clear(state: &Mutex<Loaded>) -> bool {
    let mut stored = state.lock().expect("lock the GitLab discovery state");
    let changed = stored.models.is_some() || stored.connection.is_some();
    stored.models = None;
    stored.connection = None;
    changed
}

async fn begin_authorization(
    http: Arc<dyn HttpSender>,
    env: ConfigEnv,
    default_instance_url: String,
    answer: FormAnswer,
) -> Result<OAuthAuthorization, AuthorizationError> {
    let override_client = env_value(&env, "GITLAB_OAUTH_CLIENT_ID")
        .map(|client_id| client_id.trim().to_owned())
        .filter(|client_id| !client_id.is_empty());
    let client_id = override_client.clone().unwrap_or_else(|| BUNDLED_CLIENT_ID.to_owned());
    let requested = answer
        .0
        .iter()
        .find(|(key, _)| key == "instanceUrl")
        .and_then(|(_, value)| match value {
            FormValue::String(value) => Some(value.trim().to_owned()),
            _ => None,
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| resolve_default_instance_url(&env, &default_instance_url));
    let url = url::Url::parse(&requested).map_err(|_| error("Invalid GitLab instance URL"))?;
    let instance_url = format!("{}{}", url.origin().ascii_serialization(), url.path().trim_end_matches('/'));
    let host = url_host(&url);
    if override_client.is_none() && host != GITLAB_COM_HOST {
        return Err(error(format!(
            "The bundled GitLab OAuth application only exists on {GITLAB_COM_HOST}. To sign in to {host}, register an OAuth application there with redirect URI {REDIRECT_URI} and the `{OAUTH_SCOPE}` scope (not marked \"Confidential\"), then set GITLAB_OAUTH_CLIENT_ID to its application ID."
        )));
    }
    let pkce::Pkce { verifier, challenge } = pkce::generate();
    let state = pkce::state();
    let (sender, receiver) = oneshot::channel::<Result<CallbackMessage, String>>();
    let responses: Arc<CallbackResponses> = Arc::new(Mutex::new(Some(sender)));
    let server = CallbackServer::bind_async(CALLBACK_HOST, CALLBACK_PORT, {
        let responses = responses.clone();
        let state = state.clone();
        move |request| {
            let responses = responses.clone();
            let state = state.clone();
            Box::pin(async move { handle(&state, &responses, request).await }) as HandlerFuture
        }
    })
    .await
    .map_err(bind_error)?;
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", &client_id)
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair("response_type", "code")
        .append_pair("state", &state)
        .append_pair("scope", OAUTH_SCOPE)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .finish();
    let callback: AutoCallback = Box::new({
        let http = http.clone();
        let instance_url = instance_url.clone();
        let client_id = client_id.clone();
        let verifier = verifier.clone();
        move || {
            let http = http.clone();
            let instance_url = instance_url.clone();
            let client_id = client_id.clone();
            let verifier = verifier.clone();
            Box::pin(async move {
                let _server = server;
                let message = match receiver.await {
                    Ok(Ok(message)) => message,
                    Ok(Err(message)) => return Err(error(message)),
                    Err(_) => return Err(error("Authorization failed")),
                };
                let result = exchange(
                    http.as_ref(),
                    &instance_url,
                    &client_id,
                    &[
                        ("grant_type", "authorization_code"),
                        ("code", message.code.as_str()),
                        ("redirect_uri", REDIRECT_URI),
                        ("code_verifier", verifier.as_str()),
                    ],
                    describe_grant_failure,
                )
                .await
                .map(|tokens| credential(&instance_url, &client_id, tokens));
                if let Some(respond) = message.respond {
                    let reply = match &result {
                        Ok(_) => (200, "text/html", page::success(Some(page_options()))),
                        Err(failure) => (400, "text/html", page::error(&failure.message(), Some(page_options()))),
                    };
                    let _ = respond.send(reply);
                }
                result
            })
        }
    });
    let submit: SubmitCallback = Box::new({
        let responses = responses.clone();
        let state = state.clone();
        move |text: String| {
            let responses = responses.clone();
            let state = state.clone();
            Box::pin(async move { submit_text(&state, &text, &responses) })
        }
    });
    Ok(OAuthAuthorization {
        url: format!("{instance_url}/oauth/authorize?{query}"),
        instructions: INSTRUCTIONS.to_owned(),
        expires_at: None,
        mode: OAuthMode::Auto { callback, submit: Some(submit) },
    })
}

async fn handle(state: &str, responses: &CallbackResponses, request: Request) -> Option<Reply> {
    if request.method != "GET" || request.path != CALLBACK_PATH {
        return Some((404, "", String::new()));
    }
    let params = parse_query(&request.query);
    if let Some(message) = callback_error(&params, state) {
        send(responses, Err(message.clone()));
        return Some((400, "text/html", page::error(&message, Some(page_options()))));
    }
    let code = param(&params, "code").map(str::trim).unwrap_or_default().to_owned();
    let Some(sender) = responses.lock().expect("lock the GitLab authorization callback").take() else {
        return Some((409, "", "OAuth callback already received".to_owned()));
    };
    let (respond, reply) = oneshot::channel();
    let _ = sender.send(Ok(CallbackMessage { code, respond: Some(respond) }));
    reply.await.ok()
}

fn submit_text(state: &str, text: &str, responses: &CallbackResponses) -> Result<(), AuthorizationError> {
    let pasted = redirect::pasted(text, state).map_err(|failure| error(failure.to_string()))?;
    match pasted {
        Pasted::Code { code, .. } => send(responses, Ok(CallbackMessage { code, respond: None })),
        Pasted::Error { error, .. } => send(responses, Err(error)),
    }
    Ok(())
}

fn send(responses: &CallbackResponses, outcome: Result<CallbackMessage, String>) {
    if let Some(sender) = responses.lock().expect("lock the GitLab authorization callback").take() {
        let _ = sender.send(outcome);
    }
}

async fn exchange(
    http: &dyn HttpSender,
    instance_url: &str,
    client_id: &str,
    fields: &[(&str, &str)],
    describe: fn(&str, &str) -> String,
) -> Result<Token, AuthorizationError> {
    let mut body = fields.to_vec();
    body.push(("client_id", client_id));
    match http::send(http, http::form(format!("{instance_url}/oauth/token"), Vec::new(), &body), None).await {
        Ok(response) => parse_token(&response.body),
        Err(HttpError::Status(response)) => {
            let parsed = serde_json::from_slice::<Failure>(&response.body).ok();
            let detail = parsed
                .as_ref()
                .and_then(|failure| {
                    failure
                        .error_description
                        .clone()
                        .filter(|detail| !detail.is_empty())
                        .or_else(|| failure.error.clone().filter(|detail| !detail.is_empty()))
                })
                .unwrap_or_else(|| format!("HTTP {}", response.status));
            let rejected = parsed
                .as_ref()
                .and_then(|failure| failure.error.as_deref())
                .is_some_and(|failure| failure == "invalid_grant" || failure == "invalid_client");
            Err(error(if rejected { describe(&detail, client_id) } else { detail }))
        }
        Err(_) => Err(error(REQUEST_FAILED)),
    }
}

fn parse_token(body: &[u8]) -> Result<Token, AuthorizationError> {
    let token: Token = serde_json::from_slice(body).map_err(|_| error(INVALID_RESPONSE))?;
    if token.access_token.is_empty() || token.refresh_token.is_empty() {
        return Err(error(INVALID_RESPONSE));
    }
    Ok(token)
}

fn credential(instance_url: &str, client_id: &str, tokens: Token) -> CredentialOAuth {
    let seconds = tokens.expires_in.unwrap_or(DEFAULT_EXPIRES_SECONDS);
    CredentialOAuth {
        r#type: CredentialOAuthType::Oauth,
        method_id: IntegrationMethodID(METHOD_ID.to_owned()),
        access: tokens.access_token,
        refresh: tokens.refresh_token,
        expires: opencode_util::time::now_ms() as f64 + seconds * 1000.0,
        metadata: Some(JsonMap(vec![
            ("instanceUrl".to_owned(), Value::String(instance_url.to_owned())),
            ("clientID".to_owned(), Value::String(client_id.to_owned())),
        ])),
    }
}

async fn refresh_credential(
    http: Arc<dyn HttpSender>,
    env: ConfigEnv,
    default_instance_url: String,
    value: CredentialOAuth,
) -> Result<CredentialOAuth, AuthorizationError> {
    let instance_url = credential_instance_url(value.metadata.as_ref(), &env, &default_instance_url);
    let client_id = metadata_string(value.metadata.as_ref(), "clientID")
        .filter(|client_id| !client_id.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            env_value(&env, "GITLAB_OAUTH_CLIENT_ID")
                .map(|client_id| client_id.trim().to_owned())
                .filter(|client_id| !client_id.is_empty())
        })
        .unwrap_or_else(|| LEGACY_CLIENT_ID.to_owned());
    let key = refresh_gate::key(&instance_url, &value.refresh);
    refresh_gate::run(key, async move {
        let tokens = exchange(
            http.as_ref(),
            &instance_url,
            &client_id,
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", value.refresh.as_str()),
                ("redirect_uri", REDIRECT_URI),
            ],
            describe_refresh_failure,
        )
        .await?;
        Ok(credential(&instance_url, &client_id, tokens))
    })
    .await
}

fn resolve_default_instance_url(env: &ConfigEnv, default_instance_url: &str) -> String {
    env_value(env, "GITLAB_INSTANCE_URL").unwrap_or_else(|| default_instance_url.to_owned())
}

fn credential_instance_url(metadata: Option<&JsonMap<Value>>, env: &ConfigEnv, default_instance_url: &str) -> String {
    metadata_string(metadata, "instanceUrl")
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| resolve_default_instance_url(env, default_instance_url))
}

fn metadata_instance_url(metadata: Option<&JsonMap<Value>>) -> Option<String> {
    metadata_string(metadata, "instanceUrl").map(str::to_owned)
}

fn metadata_string<'a>(metadata: Option<&'a JsonMap<Value>>, key: &str) -> Option<&'a str> {
    metadata?.0.iter().find(|(name, _)| name == key)?.1.as_str()
}

fn describe_client(client_id: &str) -> String {
    let source = if client_id == BUNDLED_CLIENT_ID {
        "bundled default"
    } else if client_id == LEGACY_CLIENT_ID {
        "opencode-gitlab-auth application"
    } else {
        "from GITLAB_OAUTH_CLIENT_ID"
    };
    format!("client_id used: {}... ({source})", client_id.chars().take(12).collect::<String>())
}

fn describe_grant_failure(detail: &str, client_id: &str) -> String {
    let custom = client_id != BUNDLED_CLIENT_ID;
    let mut hints = vec![
        "the authorization code was already used, or came from an older login attempt. Codes are single-use and bound to one PKCE verifier, so a stale browser tab or a reloaded callback page fails here. Start a completely fresh login.".to_owned(),
        format!("redirect_uri sent: {REDIRECT_URI} — the application must register this exactly"),
        describe_client(client_id),
    ];
    if custom {
        hints.push(
            "confirm that application registers the redirect URI above, grants the `api` scope, and is NOT marked \"Confidential\" (PKCE requires a public client). Unset GITLAB_OAUTH_CLIENT_ID to fall back to the bundled application.".to_owned(),
        );
    }
    format!("{detail}\n\nLikely causes:\n- {}", hints.join("\n- "))
}

fn describe_refresh_failure(detail: &str, client_id: &str) -> String {
    format!(
        "{detail}\n\nThe GitLab refresh token was revoked, expired, or issued to a different OAuth application ({}). Sign in to GitLab again.",
        describe_client(client_id)
    )
}

fn callback_error(params: &[(String, String)], state: &str) -> Option<String> {
    if param(params, "state") != Some(state) {
        return Some(INVALID_STATE.to_owned());
    }
    if param(params, "error").is_some() {
        let detail = param(params, "error_description")
            .filter(|detail| !detail.is_empty())
            .or_else(|| param(params, "error").filter(|detail| !detail.is_empty()))
            .unwrap_or(AUTHORIZATION_DENIED);
        return Some(detail.to_owned());
    }
    match param(params, "code").map(str::trim).filter(|code| !code.is_empty()) {
        Some(_) => None,
        None => Some(MISSING_CODE.to_owned()),
    }
}

fn parse_query(query: &str) -> Vec<(String, String)> {
    url::form_urlencoded::parse(query.as_bytes()).map(|(key, value)| (key.into_owned(), value.into_owned())).collect()
}

fn param<'a>(params: &'a [(String, String)], name: &str) -> Option<&'a str> {
    params.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
}

fn page_options() -> CallbackPageOptions {
    CallbackPageOptions { provider: Some("GitLab".to_owned()), auto_close: None }
}

fn bind_error(failure: io::Error) -> AuthorizationError {
    if failure.kind() == io::ErrorKind::AddrInUse {
        return error(PORT_IN_USE);
    }
    AuthorizationError::new(failure)
}

fn error(message: impl Into<String>) -> AuthorizationError {
    AuthorizationError::new(io::Error::other(message.into()))
}

fn url_host(url: &url::Url) -> String {
    let host = url.host_str().unwrap_or_default();
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    }
}

#[derive(Deserialize)]
struct Token {
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    expires_in: Option<f64>,
}

#[derive(Deserialize)]
struct Failure {
    error: Option<String>,
    error_description: Option<String>,
}

pub mod refresh_gate {
    use std::collections::HashMap;
    use std::future::Future;
    use std::sync::{LazyLock, Mutex};

    use opencode_contract::generated::CredentialOAuth;
    use tokio::sync::watch;

    use crate::integration::AuthorizationError;

    use super::{REFRESH_RETENTION_MS, error};

    pub type Outcome = Result<CredentialOAuth, AuthorizationError>;

    type Entry = (watch::Sender<Option<Outcome>>, f64);

    static REFRESHING: LazyLock<Mutex<HashMap<String, Entry>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

    pub fn key(instance_url: &str, refresh: &str) -> String {
        format!("{instance_url}\u{0}{refresh}")
    }

    pub async fn run(key: String, effect: impl Future<Output = Outcome>) -> Outcome {
        let (sender, _) = watch::channel::<Option<Outcome>>(None);
        let receiver = {
            let mut refreshing = REFRESHING.lock().expect("lock the GitLab refresh single flight");
            let now = opencode_util::time::now_ms() as f64;
            refreshing.retain(|_, (_, until)| *until >= now);
            match refreshing.get(&key) {
                Some((sender, _)) => Some(sender.subscribe()),
                None => {
                    refreshing.insert(key.clone(), (sender.clone(), f64::INFINITY));
                    None
                }
            }
        };
        let Some(mut receiver) = receiver else {
            return lead(key, sender, effect).await;
        };
        wait(&mut receiver).await
    }

    async fn lead(
        key: String,
        sender: watch::Sender<Option<Outcome>>,
        effect: impl Future<Output = Outcome>,
    ) -> Outcome {
        let mut leader = Leader { key, sender, settled: false };
        let outcome = effect.await;
        leader.sender.send_replace(Some(outcome.clone()));
        leader.settled = true;
        {
            let mut refreshing = REFRESHING.lock().expect("lock the GitLab refresh single flight");
            if outcome.is_ok() {
                let until = opencode_util::time::now_ms() as f64 + REFRESH_RETENTION_MS;
                refreshing.insert(leader.key.clone(), (leader.sender.clone(), until));
            } else {
                refreshing.remove(&leader.key);
            }
        }
        outcome
    }

    async fn wait(receiver: &mut watch::Receiver<Option<Outcome>>) -> Outcome {
        loop {
            if let Some(outcome) = receiver.borrow_and_update().clone() {
                return outcome;
            }
            if receiver.changed().await.is_err() {
                return Err(error("GitLab token refresh was cancelled"));
            }
        }
    }

    struct Leader {
        key: String,
        sender: watch::Sender<Option<Outcome>>,
        settled: bool,
    }

    impl Drop for Leader {
        fn drop(&mut self) {
            if self.settled {
                return;
            }
            let Ok(mut refreshing) = REFRESHING.lock() else {
                return;
            };
            let stored = refreshing.get(&self.key).is_some_and(|(sender, _)| sender.same_channel(&self.sender));
            if stored {
                refreshing.remove(&self.key);
            }
        }
    }
}
