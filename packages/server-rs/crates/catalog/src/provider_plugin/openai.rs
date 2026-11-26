use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use opencode_ai::HttpSender;
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    CredentialOAuth, CredentialOAuthType, CredentialValue, FormAnswer, IntegrationID, IntegrationMethodID,
    IntegrationOAuthMethod, IntegrationOAuthMethodType, ProviderEncoding, ProviderID, ProviderSettings,
    ProviderTransport,
};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::oneshot;

use crate::aisdk_native;
use crate::builtin::{Builtin, Services};
use crate::integration::{
    AuthorizationError, AutoCallback, Implementation, OAuthAuthorization, OAuthFuture, OAuthImplementation, OAuthMode,
    RefreshCallback, SubmitCallback,
};
use crate::model;
use crate::oauth::callback::{CallbackServer, HandlerFuture, Reply, Request};
use crate::oauth::page::{self, CallbackPageOptions};
use crate::oauth::pkce;
use crate::oauth::redirect::{self, Pasted};
use crate::provider;
use crate::provider_plugin::http::{self, HttpError, PreparedHttp};
use crate::provider_plugin::{PluginContext, refresh, settings};

pub const ID: &str = "opencode.provider.openai";

const INTEGRATION_ID: &str = "openai";

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

const ISSUER: &str = "https://auth.openai.com";

const TOKEN_PATH: &str = "/oauth/token";

const AUTHORIZE_PATH: &str = "/oauth/authorize";

const DEVICE_PATH: &str = "/codex/device";

const DEVICE_CODE_PATH: &str = "/api/accounts/deviceauth/usercode";

const DEVICE_TOKEN_PATH: &str = "/api/accounts/deviceauth/token";

const DEVICE_REDIRECT: &str = "https://auth.openai.com/deviceauth/callback";

const CALLBACK_HOST: &str = "localhost";

const CALLBACK_PORT: u16 = 1455;

const CALLBACK_FALLBACK_PORT: u16 = 1457;

const CALLBACK_BIND_ATTEMPTS: u32 = 10;

const CALLBACK_BIND_RETRY_DELAY: Duration = Duration::from_millis(200);

const CANCEL_TIMEOUT: Duration = Duration::from_secs(2);

const CALLBACK_PATH: &str = "/auth/callback";

const SCOPE: &str = "openid profile email offline_access";

const BROWSER_METHOD_ID: &str = "chatgpt-browser";

const HEADLESS_METHOD_ID: &str = "chatgpt-headless";

const BROWSER_LABEL: &str = "Codex browser (legacy)";

const HEADLESS_LABEL: &str = "Codex device code (legacy)";

const INSTRUCTIONS: &str = "Complete authorization in your browser. This window will close automatically.";

const POLLING_SAFETY_MARGIN_MS: u64 = 3000;

const CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";

const CODEX_DISALLOWED: [&str; 1] = ["gpt-5.6"];

const CODEX_IMAGE_MODELS: [&str; 1] = ["gpt-image-2"];

const PORT_IN_USE: &str = "OpenAI browser login needs local port 1455 or 1457, but both are already in use. Stop the processes using those ports or choose Codex device code (legacy), then try again.";

const JSON: &str = "application/json";

const FORM: &str = "application/x-www-form-urlencoded";

const DEFAULT_EXPIRES_SECONDS: f64 = 3600.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flow {
    Browser,
    Headless,
}

#[derive(Deserialize)]
struct TokenResponse {
    #[serde(default)]
    id_token: String,
    access_token: String,
    refresh_token: String,
    expires_in: Option<f64>,
}

#[derive(Deserialize)]
struct DeviceCode {
    device_auth_id: String,
    user_code: String,
    interval: String,
}

#[derive(Deserialize)]
struct DeviceToken {
    authorization_code: String,
    code_verifier: String,
}

struct OpenAIOAuth {
    integration_id: IntegrationID,
    method: IntegrationOAuthMethod,
    http: Arc<dyn HttpSender>,
    user_agent: String,
    flow: Flow,
}

impl OAuthImplementation for OpenAIOAuth {
    fn integration_id(&self) -> &IntegrationID {
        &self.integration_id
    }

    fn method(&self) -> &IntegrationOAuthMethod {
        &self.method
    }

    fn authorize(&self, _answer: FormAnswer) -> OAuthFuture<OAuthAuthorization> {
        match self.flow {
            Flow::Browser => Box::pin(begin_browser(self.http.clone(), self.user_agent.clone())),
            Flow::Headless => Box::pin(begin_headless(self.http.clone(), self.user_agent.clone())),
        }
    }

    fn refresh(&self) -> Option<RefreshCallback> {
        let http = self.http.clone();
        let user_agent = self.user_agent.clone();
        let method_id = self.method.id.0.clone();
        Some(Box::new(move |value: CredentialOAuth| {
            let http = http.clone();
            let user_agent = user_agent.clone();
            let method_id = method_id.clone();
            Box::pin(async move { refresh_credential(http.as_ref(), &user_agent, &method_id, value).await })
        }))
    }
}

type CallbackResponses = Mutex<Option<oneshot::Sender<Result<String, String>>>>;

pub async fn register(services: &Services, context: &PluginContext, builtin: &mut Builtin) {
    let provider_id = ProviderID(INTEGRATION_ID.to_owned());
    let integration_id = IntegrationID(INTEGRATION_ID.to_owned());

    let integration_registration = services
        .integration
        .transform({
            let http = context.http.clone();
            let user_agent = context.app.useragent();
            move |editor| {
                for (method_id, label, flow) in [
                    (BROWSER_METHOD_ID, BROWSER_LABEL, Flow::Browser),
                    (HEADLESS_METHOD_ID, HEADLESS_LABEL, Flow::Headless),
                ] {
                    editor.method_update(Implementation::OAuth(Arc::new(OpenAIOAuth {
                        integration_id: IntegrationID(INTEGRATION_ID.to_owned()),
                        method: IntegrationOAuthMethod {
                            id: IntegrationMethodID(method_id.to_owned()),
                            r#type: IntegrationOAuthMethodType::Oauth,
                            label: label.to_owned(),
                            form: None,
                        },
                        http: http.clone(),
                        user_agent: user_agent.clone(),
                        flow,
                    })));
                }
                Ok(())
            }
        })
        .await;
    builtin.registrations.push(integration_registration);

    let chatgpt: Arc<Mutex<Option<CredentialOAuth>>> = Arc::new(Mutex::new(None));
    let (_, credential) = settings::stored_credential(services, &integration_id).await;
    apply(&chatgpt, credential);

    let configured = settings::configured_settings(services, &provider_id).await.ok().flatten();

    let provider_registration = services
        .provider
        .transform({
            let chatgpt = chatgpt.clone();
            let provider_id = provider_id.clone();
            move |editor| {
                if editor.get(&provider_id).is_none() {
                    return Ok(());
                }
                let active = chatgpt.lock().expect("lock the OpenAI ChatGPT credential").clone();
                let account = active.as_ref().and_then(account_id).map(str::to_owned);
                editor.update(&provider_id, |provider| {
                    let transport = provider
                        .settings
                        .as_ref()
                        .and_then(|settings| settings.transport)
                        .unwrap_or(ProviderTransport::Websocket);
                    let encoding = match (&active, super::setting(configured.as_ref(), "baseURL")) {
                        (Some(_), None) => Some(
                            provider
                                .settings
                                .as_ref()
                                .and_then(|settings| settings.encoding)
                                .unwrap_or(ProviderEncoding::Zstd),
                        ),
                        _ => None,
                    };
                    let rest = match active {
                        Some(_) => vec![("baseURL".to_owned(), Value::String(CODEX_BASE_URL.to_owned()))],
                        None => Vec::new(),
                    };
                    let overlay = ProviderSettings {
                        timeout: None,
                        header_timeout: None,
                        chunk_timeout: None,
                        compaction: None,
                        cache: None,
                        transport: Some(transport),
                        encoding,
                        tls: None,
                        rest: JsonMap(rest),
                    };
                    let base = provider.settings.as_ref().map(provider::json_object);
                    let overlay = provider::json_object(&overlay);
                    provider.settings =
                        aisdk_native::merge_overlay(base.as_ref(), Some(&overlay)).and_then(provider::decode_object);
                    if active.is_none() {
                        return;
                    }
                    let mut headers = aisdk_native::Headers::new();
                    headers.insert("originator".to_owned(), "opencode".to_owned());
                    headers.insert("x-codex-beta-features".to_owned(), "remote_compaction_v2".to_owned());
                    if let Some(account) = &account {
                        headers.insert("chatgpt-account-id".to_owned(), account.clone());
                    }
                    let base = provider.headers.as_ref().map(provider::headers_to_native);
                    provider.headers =
                        aisdk_native::merge_headers(base.as_ref(), Some(&headers)).map(provider::headers_from_native);
                });
                Ok(())
            }
        })
        .await;
    builtin.registrations.push(provider_registration);

    let model_registration = services
        .model
        .transform({
            let chatgpt = chatgpt.clone();
            let provider_id = provider_id.clone();
            move |editor| {
                if chatgpt.lock().expect("lock the OpenAI ChatGPT credential").is_none() {
                    return Ok(());
                }
                for model in editor.list(Some(&provider_id)) {
                    let api_id = model.model_id.clone();
                    if reasoning_mode_is_pro(model.body.as_ref()) {
                        model.enabled = false;
                        continue;
                    }
                    if model::type_of(model) == "image" {
                        if CODEX_IMAGE_MODELS.contains(&api_id.as_str()) {
                            model.cost = Vec::new();
                        } else {
                            model.enabled = false;
                        }
                        continue;
                    }
                    let eligible =
                        version(&api_id).is_some_and(|(major, minor)| major > 5 || (major == 5 && minor > 5));
                    if !eligible || CODEX_DISALLOWED.contains(&api_id.as_str()) {
                        model.enabled = false;
                        continue;
                    }
                    model.cost = Vec::new();
                }
                Ok(())
            }
        })
        .await;
    builtin.registrations.push(model_registration);

    let hooks = context.hooks.clone();
    builtin.hooks.push(hooks.on_model_request(Some(provider_id.clone()), {
        let chatgpt = chatgpt.clone();
        let sessions = context.sessions.clone();
        move |mut event| {
            let chatgpt = chatgpt.clone();
            let sessions = sessions.clone();
            Box::pin(async move {
                let active = chatgpt.lock().expect("lock the OpenAI ChatGPT credential").is_some();
                if !active {
                    return event;
                }
                if let Some(base_url) = event.base_url.clone()
                    && is_openai_origin(&base_url)
                {
                    event.base_url = Some(CODEX_BASE_URL.to_owned());
                }
                let affinity = match sessions.session(event.session_id.clone()).await {
                    Some(session) => session.affinity().0,
                    None => event.session_id.0.clone(),
                };
                event.headers.insert("originator".to_owned(), "opencode".to_owned());
                event.headers.insert("session-id".to_owned(), affinity);
                event
            })
        }
    }));
    builtin.hooks.push(hooks.on_context(Some(provider_id.clone()), |mut event| {
        Box::pin(async move {
            event.options.shift_remove("maxTokens");
            event
        })
    }));
    builtin.hooks.push(hooks.on_compaction(Some(provider_id), |mut event| {
        Box::pin(async move {
            event.options.shift_remove("maxTokens");
            event
        })
    }));

    let switch = refresh::on_switch(&services.bus, INTEGRATION_ID, {
        let integrations = services.integration.clone();
        let credentials = services.credentials.clone();
        let providers = services.provider.clone();
        let chatgpt = chatgpt.clone();
        let integration_id = integration_id.clone();
        move || {
            let integrations = integrations.clone();
            let credentials = credentials.clone();
            let providers = providers.clone();
            let chatgpt = chatgpt.clone();
            let integration_id = integration_id.clone();
            Box::pin(async move {
                let (_, credential) =
                    settings::read_stored_credential(&integrations, &credentials, &integration_id).await;
                if apply(&chatgpt, credential) {
                    providers.reload().await;
                }
            })
        }
    });
    builtin.tasks.push(switch);
}

fn apply(chatgpt: &Arc<Mutex<Option<CredentialOAuth>>>, credential: Option<CredentialValue>) -> bool {
    let next = match credential {
        Some(CredentialValue::Oauth(value)) if is_chatgpt_method(&value.method_id) => Some(value),
        _ => None,
    };
    let mut current = chatgpt.lock().expect("lock the OpenAI ChatGPT credential");
    let changed = current.is_some() != next.is_some()
        || current.as_ref().and_then(account_id) != next.as_ref().and_then(account_id);
    *current = next;
    changed
}

fn is_chatgpt_method(method_id: &IntegrationMethodID) -> bool {
    method_id.0 == BROWSER_METHOD_ID || method_id.0 == HEADLESS_METHOD_ID
}

fn account_id(credential: &CredentialOAuth) -> Option<&str> {
    credential.metadata.as_ref()?.0.iter().find(|(key, _)| key == "accountID")?.1.as_str()
}

fn is_openai_origin(base_url: &str) -> bool {
    url::Url::parse(base_url).is_ok_and(|url| url.origin().ascii_serialization() == "https://api.openai.com")
}

fn reasoning_mode_is_pro(body: Option<&JsonMap<Value>>) -> bool {
    body.and_then(|body| body.0.iter().find(|(key, _)| key == "reasoning"))
        .and_then(|(_, reasoning)| reasoning.get("mode"))
        .and_then(Value::as_str)
        == Some("pro")
}

fn version(api_id: &str) -> Option<(u128, u128)> {
    let rest = api_id.strip_prefix("gpt-")?;
    let end = rest.find(|character: char| !character.is_ascii_digit()).unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    let major = rest[..end].parse().unwrap_or(u128::MAX);
    let minor = rest[end..]
        .strip_prefix('.')
        .and_then(|rest| {
            let end = rest.find(|character: char| !character.is_ascii_digit()).unwrap_or(rest.len());
            (end > 0).then(|| rest[..end].parse().unwrap_or(u128::MAX))
        })
        .unwrap_or(0);
    Some((major, minor))
}

async fn begin_browser(
    http: Arc<dyn HttpSender>,
    user_agent: String,
) -> Result<OAuthAuthorization, AuthorizationError> {
    let pkce::Pkce { verifier, challenge } = pkce::generate();
    let state = pkce::state();
    let (sender, receiver) = oneshot::channel::<Result<String, String>>();
    let responses: Arc<CallbackResponses> = Arc::new(Mutex::new(Some(sender)));
    let (server, port) = listen(http.as_ref(), &responses, &state).await?;
    let redirect = format!("http://localhost:{port}{CALLBACK_PATH}");
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("response_type", "code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", &redirect)
        .append_pair("scope", SCOPE)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("state", &state)
        .append_pair("originator", "opencode")
        .finish();
    let callback: AutoCallback = Box::new({
        let http = http.clone();
        let user_agent = user_agent.clone();
        let verifier = verifier.clone();
        let redirect = redirect.clone();
        move || {
            let http = http.clone();
            let user_agent = user_agent.clone();
            let verifier = verifier.clone();
            let redirect = redirect.clone();
            Box::pin(async move {
                let _server = server;
                let code = match receiver.await {
                    Ok(Ok(code)) => code,
                    Ok(Err(message)) => return Err(error(message)),
                    Err(_) => return Err(error("Authorization failed")),
                };
                let tokens = exchange(http.as_ref(), &user_agent, &code, &redirect, &verifier).await?;
                Ok(credential(BROWSER_METHOD_ID, &tokens))
            })
        }
    });
    let submit: SubmitCallback = Box::new({
        let responses = responses.clone();
        let state = state.clone();
        move |pasted: String| {
            let responses = responses.clone();
            let state = state.clone();
            Box::pin(async move { submit_text(&state, &pasted, &responses) })
        }
    });
    Ok(OAuthAuthorization {
        url: format!("{ISSUER}{AUTHORIZE_PATH}?{query}"),
        instructions: INSTRUCTIONS.to_owned(),
        expires_at: None,
        mode: OAuthMode::Auto { callback, submit: Some(submit) },
    })
}

async fn listen(
    http: &dyn HttpSender,
    responses: &Arc<CallbackResponses>,
    state: &str,
) -> Result<(CallbackServer, u16), AuthorizationError> {
    match bind(CALLBACK_PORT, responses, state).await {
        Ok(server) => return Ok((server, CALLBACK_PORT)),
        Err(failure) if failure.kind() == io::ErrorKind::AddrInUse => {}
        Err(failure) => return Err(AuthorizationError::new(failure)),
    }
    let cancel = http::get(format!("http://localhost:{CALLBACK_PORT}/cancel"), Vec::new());
    let _ = http::send(http, cancel, Some(CANCEL_TIMEOUT)).await;
    tokio::time::sleep(CALLBACK_BIND_RETRY_DELAY).await;
    match bind_with_retry(CALLBACK_PORT, CALLBACK_BIND_ATTEMPTS - 1, responses, state).await {
        Ok(server) => return Ok((server, CALLBACK_PORT)),
        Err(failure) if failure.kind() == io::ErrorKind::AddrInUse => {}
        Err(failure) => return Err(AuthorizationError::new(failure)),
    }
    match bind_with_retry(CALLBACK_FALLBACK_PORT, CALLBACK_BIND_ATTEMPTS, responses, state).await {
        Ok(server) => Ok((server, CALLBACK_FALLBACK_PORT)),
        Err(failure) if failure.kind() == io::ErrorKind::AddrInUse => Err(error(PORT_IN_USE)),
        Err(failure) => Err(AuthorizationError::new(failure)),
    }
}

async fn bind_with_retry(
    port: u16,
    attempts: u32,
    responses: &Arc<CallbackResponses>,
    state: &str,
) -> io::Result<CallbackServer> {
    let mut attempts = attempts;
    loop {
        match bind(port, responses, state).await {
            Ok(server) => return Ok(server),
            Err(failure) if failure.kind() == io::ErrorKind::AddrInUse && attempts > 1 => {
                tokio::time::sleep(CALLBACK_BIND_RETRY_DELAY).await;
                attempts -= 1;
            }
            Err(failure) => return Err(failure),
        }
    }
}

async fn bind(port: u16, responses: &Arc<CallbackResponses>, state: &str) -> io::Result<CallbackServer> {
    let responses = responses.clone();
    let state = state.to_owned();
    CallbackServer::bind_async(CALLBACK_HOST, port, move |request| {
        let responses = responses.clone();
        let state = state.clone();
        Box::pin(async move { Some(handle(&state, &responses, request)) }) as HandlerFuture
    })
    .await
}

fn handle(state: &str, responses: &CallbackResponses, request: Request) -> Reply {
    if request.path != CALLBACK_PATH {
        return (404, "", "Not found".to_owned());
    }
    let params = parse_query(&request.query);
    if param(&params, "state") != Some(state) {
        return (400, "text/html", page::error("Invalid OAuth state", Some(page_options())));
    }
    let failure = match param(&params, "error_description") {
        Some(detail) => Some(detail),
        None => param(&params, "error"),
    }
    .filter(|detail| !detail.is_empty());
    if let Some(failure) = failure {
        respond(responses, Err(failure.to_owned()));
        return (400, "text/html", page::error(failure, Some(page_options())));
    }
    let Some(code) = param(&params, "code").filter(|code| !code.is_empty()) else {
        respond(responses, Err("Missing authorization code".to_owned()));
        return (400, "text/html", page::error("Missing authorization code", Some(page_options())));
    };
    respond(responses, Ok(code.to_owned()));
    (200, "text/html", page::success(Some(page_options())))
}

fn submit_text(state: &str, text: &str, responses: &CallbackResponses) -> Result<(), AuthorizationError> {
    let pasted = redirect::pasted(text, state).map_err(|failure| error(failure.to_string()))?;
    match pasted {
        Pasted::Code { code, .. } => respond(responses, Ok(code)),
        Pasted::Error { error: message, .. } => respond(responses, Err(message)),
    }
    Ok(())
}

fn respond(responses: &CallbackResponses, outcome: Result<String, String>) {
    if let Some(sender) = responses.lock().expect("lock the OpenAI authorization callback").take() {
        let _ = sender.send(outcome);
    }
}

fn parse_query(query: &str) -> Vec<(String, String)> {
    url::form_urlencoded::parse(query.as_bytes()).map(|(key, value)| (key.into_owned(), value.into_owned())).collect()
}

fn param<'a>(params: &'a [(String, String)], name: &str) -> Option<&'a str> {
    params.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
}

fn page_options() -> CallbackPageOptions {
    CallbackPageOptions { provider: Some("ChatGPT".to_owned()), auto_close: None }
}

async fn begin_headless(
    http: Arc<dyn HttpSender>,
    user_agent: String,
) -> Result<OAuthAuthorization, AuthorizationError> {
    let device: DeviceCode = send_request(
        http.as_ref(),
        http::json(
            format!("{ISSUER}{DEVICE_CODE_PATH}"),
            headers(JSON, &user_agent),
            &serde_json::json!({ "client_id": CLIENT_ID }),
        ),
    )
    .await?;
    let interval = Duration::from_millis(interval_millis(&device.interval) + POLLING_SAFETY_MARGIN_MS);
    let instructions = format!("Enter code: {}", device.user_code);
    let callback: AutoCallback = Box::new({
        let http = http.clone();
        let user_agent = user_agent.clone();
        move || {
            let http = http.clone();
            let user_agent = user_agent.clone();
            Box::pin(async move {
                loop {
                    let request = http::json(
                        format!("{ISSUER}{DEVICE_TOKEN_PATH}"),
                        headers(JSON, &user_agent),
                        &serde_json::json!({
                            "device_auth_id": &device.device_auth_id,
                            "user_code": &device.user_code,
                        }),
                    );
                    match http::send(http.as_ref(), request, None).await {
                        Ok(response) => {
                            let granted: DeviceToken = serde_json::from_slice(&response.body)
                                .map_err(|failure| error(format!("Request failed: {failure}")))?;
                            let tokens = exchange(
                                http.as_ref(),
                                &user_agent,
                                &granted.authorization_code,
                                DEVICE_REDIRECT,
                                &granted.code_verifier,
                            )
                            .await?;
                            return Ok(credential(HEADLESS_METHOD_ID, &tokens));
                        }
                        Err(HttpError::Status(response)) if response.status == 403 || response.status == 404 => {
                            tokio::time::sleep(interval).await;
                        }
                        Err(HttpError::Status(response)) => {
                            return Err(error(format!("Device authorization failed: {}", response.status)));
                        }
                        Err(failure) => return Err(error(format!("Device authorization failed: {failure}"))),
                    }
                }
            })
        }
    });
    Ok(OAuthAuthorization {
        url: format!("{ISSUER}{DEVICE_PATH}"),
        instructions,
        expires_at: None,
        mode: OAuthMode::Auto { callback, submit: None },
    })
}

async fn exchange(
    http: &dyn HttpSender,
    user_agent: &str,
    code: &str,
    redirect: &str,
    verifier: &str,
) -> Result<TokenResponse, AuthorizationError> {
    let request = http::form(
        format!("{ISSUER}{TOKEN_PATH}"),
        headers(FORM, user_agent),
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect),
            ("client_id", CLIENT_ID),
            ("code_verifier", verifier),
        ],
    );
    send_request(http, request).await
}

async fn refresh_credential(
    http: &dyn HttpSender,
    user_agent: &str,
    method_id: &str,
    value: CredentialOAuth,
) -> Result<CredentialOAuth, AuthorizationError> {
    let request = http::form(
        format!("{ISSUER}{TOKEN_PATH}"),
        headers(FORM, user_agent),
        &[("grant_type", "refresh_token"), ("refresh_token", &value.refresh), ("client_id", CLIENT_ID)],
    );
    let tokens: TokenResponse = send_request(http, request).await?;
    let mut next = credential(method_id, &tokens);
    if next.metadata.is_none() {
        next.metadata = value.metadata;
    }
    Ok(next)
}

async fn send_request<T: serde::de::DeserializeOwned>(
    http: &dyn HttpSender,
    request: PreparedHttp,
) -> Result<T, AuthorizationError> {
    match http::send(http, request, None).await {
        Ok(response) => {
            serde_json::from_slice(&response.body).map_err(|failure| error(format!("Request failed: {failure}")))
        }
        Err(HttpError::Status(response)) => Err(error(format!("Request failed: {}", response.status))),
        Err(failure) => Err(error(format!("Request failed: {failure}"))),
    }
}

fn headers(content_type: &str, user_agent: &str) -> Vec<(String, String)> {
    vec![("content-type".to_owned(), content_type.to_owned()), ("user-agent".to_owned(), user_agent.to_owned())]
}

fn credential(method_id: &str, tokens: &TokenResponse) -> CredentialOAuth {
    let metadata =
        extract_account_id(tokens).map(|account| JsonMap(vec![("accountID".to_owned(), Value::String(account))]));
    CredentialOAuth {
        r#type: CredentialOAuthType::Oauth,
        method_id: IntegrationMethodID(method_id.to_owned()),
        refresh: tokens.refresh_token.clone(),
        access: tokens.access_token.clone(),
        expires: opencode_util::time::now_ms() as f64 + tokens.expires_in.unwrap_or(DEFAULT_EXPIRES_SECONDS) * 1000.0,
        metadata,
    }
}

fn extract_account_id(tokens: &TokenResponse) -> Option<String> {
    claim(&tokens.id_token).or_else(|| claim(&tokens.access_token))
}

fn claim(token: &str) -> Option<String> {
    let part = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(part).or_else(|_| URL_SAFE.decode(part)).ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    claims
        .get("chatgpt_account_id")
        .and_then(Value::as_str)
        .or_else(|| {
            claims
                .get("https://api.openai.com/auth")
                .and_then(|auth| auth.get("chatgpt_account_id"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            claims
                .get("organizations")
                .and_then(Value::as_array)
                .and_then(|organizations| organizations.first())
                .and_then(|organization| organization.get("id"))
                .and_then(Value::as_str)
        })
        .map(str::to_owned)
}

fn interval_millis(value: &str) -> u64 {
    let text = value.trim_start();
    let (sign, text) = match text.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, text.strip_prefix('+').unwrap_or(text)),
    };
    let end = text.find(|character: char| !character.is_ascii_digit()).unwrap_or(text.len());
    let seconds = match text[..end].parse::<i64>() {
        Ok(0) | Err(_) => 5,
        Ok(seconds) => seconds * sign,
    };
    (seconds.max(1) as u64).saturating_mul(1000)
}

fn error(message: impl Into<String>) -> AuthorizationError {
    AuthorizationError::new(io::Error::other(message.into()))
}
