use std::io;
use std::sync::{Arc, LazyLock, Mutex};

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use opencode_ai::HttpSender;
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    ConnectionInfo, CredentialOAuth, CredentialOAuthType, CredentialValue, FormAnswer, FormField1, FormFields1,
    FormStringField1, FormStringField1Type, FormValue, IntegrationEnvMethod, IntegrationEnvMethodType, IntegrationID,
    IntegrationKeyMethod, IntegrationKeyMethodType, IntegrationMethod, IntegrationMethodID, IntegrationOAuthMethod,
    IntegrationOAuthMethodType, ModelCacheFormat, ModelCompatibility, ModelMaxTokensField, ProviderID,
    ProviderSettings,
};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use regex_lite::Regex;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::oneshot;

use crate::builtin::{Builtin, Services};
use crate::credential::{CredentialService, Update};
use crate::integration::{
    AuthorizationError, AutoCallback, ConfigEnv, Implementation, IntegrationService, LabelCallback, OAuthAuthorization,
    OAuthFuture, OAuthImplementation, OAuthMode, RefreshCallback, SubmitCallback,
};
use crate::oauth::callback::{CallbackServer, Reply, Request};
use crate::oauth::page::{self, CallbackPageOptions};
use crate::oauth::pkce;
use crate::oauth::redirect::{self, Pasted};
use crate::provider;
use crate::provider_plugin::hooks::{HttpRequest, HttpResponse, RetryDecision};
use crate::provider_plugin::http::{self, BufferedResponse, HttpError};
use crate::provider_plugin::{PluginContext, env_value, refresh, setting, settings};

pub const ID: &str = "opencode.provider.snowflake.cortex";

const PROVIDER_ID: &str = "snowflake-cortex";

const INTEGRATION_ID: &str = "snowflake-cortex";

const METHOD_ID: &str = "browser";

const CLIENT_ID: &str = "LOCAL_APPLICATION";

const INSTRUCTIONS: &str = "Complete Snowflake sign-in in your browser.";

const CALLBACK_HOST: &str = "127.0.0.1";

const DEFAULT_EXPIRY_SECONDS: f64 = 600.0;

const INVALID_STATE: &str = "Invalid OAuth state";

const MISSING_CODE: &str = "Missing authorization code";

const AUTHORIZATION_FAILED: &str = "Authorization failed";

const CONVERSATION_COMPLETE_BODY: &[u8] =
    b"data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";

const ENCODE_URI_COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

static SCHEME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^https?://").expect("the scheme pattern is valid"));

static TRAILING_SLASHES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/+$").expect("the trailing slash pattern is valid"));

static TRAILING_DOMAIN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\.snowflakecomputing\.com$").expect("the domain pattern is valid"));

static ROLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[-_A-Za-z0-9]+$").expect("the role pattern is valid"));

#[derive(Deserialize)]
struct Token {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<f64>,
}

struct SnowflakeOAuth {
    integration_id: IntegrationID,
    method: IntegrationOAuthMethod,
    http: Arc<dyn HttpSender>,
    user_agent: String,
    env: ConfigEnv,
}

impl OAuthImplementation for SnowflakeOAuth {
    fn integration_id(&self) -> &IntegrationID {
        &self.integration_id
    }

    fn method(&self) -> &IntegrationOAuthMethod {
        &self.method
    }

    fn authorize(&self, answer: FormAnswer) -> OAuthFuture<OAuthAuthorization> {
        Box::pin(begin_authorization(self.http.clone(), self.user_agent.clone(), answer))
    }

    fn refresh(&self) -> Option<RefreshCallback> {
        let http = self.http.clone();
        let user_agent = self.user_agent.clone();
        let env = self.env.clone();
        Some(Box::new(move |value: CredentialOAuth| {
            let http = http.clone();
            let user_agent = user_agent.clone();
            let env = env.clone();
            Box::pin(async move {
                if env_token(&env).is_some() {
                    return Ok(value);
                }
                let account = normalize_account(account_metadata(value.metadata.as_ref()).and_then(Value::as_str));
                token(
                    http.as_ref(),
                    &user_agent,
                    &account,
                    &[("grant_type", "refresh_token"), ("refresh_token", value.refresh.as_str())],
                )
                .await
            })
        }))
    }

    fn label(&self) -> Option<LabelCallback> {
        Some(Box::new(|value| {
            Some(normalize_account(account_metadata(value.metadata.as_ref()).and_then(Value::as_str)))
        }))
    }
}

type CallbackSender = oneshot::Sender<Result<String, String>>;

type CallbackResponses = Mutex<Option<CallbackSender>>;

pub async fn register(services: &Services, context: &PluginContext, builtin: &mut Builtin) {
    let provider_id = ProviderID(PROVIDER_ID.to_owned());
    let integration_id = IntegrationID(INTEGRATION_ID.to_owned());

    let integration_registration = services
        .integration
        .transform({
            let http = context.http.clone();
            let user_agent = context.app.useragent();
            let env = context.env.clone();
            move |editor| {
                let id = IntegrationID(INTEGRATION_ID.to_owned());
                for method in [
                    IntegrationMethod::Key(IntegrationKeyMethod {
                        r#type: IntegrationKeyMethodType::Key,
                        label: Some("Paste PAT or bearer token".to_owned()),
                        form: Some(account_form()),
                    }),
                    IntegrationMethod::Env(IntegrationEnvMethod {
                        r#type: IntegrationEnvMethodType::Env,
                        names: vec!["SNOWFLAKE_CORTEX_TOKEN".to_owned(), "SNOWFLAKE_CORTEX_PAT".to_owned()],
                    }),
                ] {
                    editor.method_update(Implementation::Method { integration_id: id.clone(), method });
                }
                editor.method_update(Implementation::OAuth(Arc::new(SnowflakeOAuth {
                    integration_id: id.clone(),
                    method: IntegrationOAuthMethod {
                        id: IntegrationMethodID(METHOD_ID.to_owned()),
                        r#type: IntegrationOAuthMethodType::Oauth,
                        label: "Login with Snowflake (External Browser)".to_owned(),
                        form: Some(oauth_form()),
                    },
                    http: http.clone(),
                    user_agent: user_agent.clone(),
                    env: env.clone(),
                })));
                Ok(())
            }
        })
        .await;
    builtin.registrations.push(integration_registration);

    let configured = settings::configured_settings(services, &provider_id).await.ok().flatten();
    let account = Arc::new(Mutex::new(String::new()));
    load_account(
        &services.integration,
        &services.credentials,
        &context.env,
        configured.as_ref(),
        &integration_id,
        &account,
    )
    .await;

    let provider_registration = services
        .provider
        .transform({
            let provider_id = provider_id.clone();
            let configured = configured.clone();
            let account = account.clone();
            move |editor| {
                if editor.get(&provider_id).is_none() {
                    return Ok(());
                }
                let configured = configured.clone();
                let account = account.lock().expect("lock the Snowflake account").clone();
                editor.update(&provider_id, |provider| {
                    let base = provider.settings.as_ref().map(provider::json_object);
                    let overlay = configured.as_ref().map(provider::json_object);
                    let mut settings =
                        crate::aisdk_native::merge_overlay(base.as_ref(), overlay.as_ref()).unwrap_or_default();
                    let current = settings.get("baseURL").and_then(Value::as_str).map(str::to_owned);
                    settings.insert("baseURL".to_owned(), Value::String(endpoint(current.as_deref(), &account)));
                    if let Some(token) = settings.get("token").and_then(Value::as_str) {
                        settings.insert("apiKey".to_owned(), Value::String(token.to_owned()));
                    }
                    provider.settings = Some(provider::decode_object::<ProviderSettings>(settings));
                });
                Ok(())
            }
        })
        .await;
    builtin.registrations.push(provider_registration);

    let model_registration = services
        .model
        .transform({
            let provider_id = provider_id.clone();
            let account = account.clone();
            move |editor| {
                let account = account.lock().expect("lock the Snowflake account").clone();
                for model in editor.list(Some(&provider_id)) {
                    let claude = model.model_id.contains("claude");
                    let compatibility = model.compatibility.get_or_insert_with(empty_compatibility);
                    compatibility.max_tokens_field.get_or_insert(ModelMaxTokensField::MaxCompletionTokens);
                    if claude {
                        compatibility.cache_format.get_or_insert(ModelCacheFormat::Anthropic);
                    }
                    if let Some(model_settings) = model.settings.as_mut()
                        && let Some((_, value)) = model_settings.rest.0.iter_mut().find(|(key, _)| key == "baseURL")
                    {
                        let base = value.as_str().map(str::to_owned);
                        *value = Value::String(endpoint(base.as_deref(), &account));
                    }
                }
                Ok(())
            }
        })
        .await;
    builtin.registrations.push(model_registration);

    let switch = refresh::on_switch(&services.bus, INTEGRATION_ID, {
        let integrations = services.integration.clone();
        let credentials = services.credentials.clone();
        let providers = services.provider.clone();
        let environment = context.env.clone();
        let configured = configured.clone();
        let account = account.clone();
        let integration_id = integration_id.clone();
        move || {
            let integrations = integrations.clone();
            let credentials = credentials.clone();
            let providers = providers.clone();
            let environment = environment.clone();
            let configured = configured.clone();
            let account = account.clone();
            let integration_id = integration_id.clone();
            Box::pin(async move {
                let changed = load_account(
                    &integrations,
                    &credentials,
                    &environment,
                    configured.as_ref(),
                    &integration_id,
                    &account,
                )
                .await;
                if changed {
                    providers.reload().await;
                }
            })
        }
    });
    builtin.tasks.push(switch);

    let hooks = context.hooks.clone();
    builtin.hooks.push(hooks.on_http_request(Some(provider_id.clone()), {
        let environment = context.env.clone();
        let user_agent = context.app.useragent();
        move |mut event: HttpRequest| {
            let environment = environment.clone();
            let user_agent = user_agent.clone();
            Box::pin(async move {
                if let Some(token) = env_token(&environment) {
                    upsert(&mut event.request.headers, "authorization", format!("Bearer {token}"));
                }
                upsert(&mut event.request.headers, "user-agent", user_agent);
                event
            })
        }
    }));
    builtin.hooks.push(hooks.on_http_response(Some(provider_id.clone()), move |mut event: HttpResponse| {
        Box::pin(async move {
            if event.response.status != 400 {
                return event;
            }
            let message = error_message(&event.response.body);
            if !message.to_lowercase().contains("conversation complete") {
                return event;
            }
            event.response = BufferedResponse {
                status: 200,
                headers: vec![("content-type".to_owned(), "text/event-stream".to_owned())],
                body: bytes::Bytes::from_static(CONVERSATION_COMPLETE_BODY),
            };
            event
        })
    }));
    builtin.hooks.push(hooks.on_retry(Some(provider_id.clone()), {
        let integrations = services.integration.clone();
        let credentials = services.credentials.clone();
        let http = context.http.clone();
        let user_agent = context.app.useragent();
        let environment = context.env.clone();
        let integration_id = integration_id.clone();
        move |mut event| {
            let integrations = integrations.clone();
            let credentials = credentials.clone();
            let http = http.clone();
            let user_agent = user_agent.clone();
            let environment = environment.clone();
            let integration_id = integration_id.clone();
            Box::pin(async move {
                if event.error.get("status").and_then(Value::as_u64) != Some(401)
                    || event.attempt != 2
                    || env_token(&environment).is_some()
                {
                    return event;
                }
                let (connection, credential) =
                    settings::read_stored_credential(&integrations, &credentials, &integration_id).await;
                let Some(ConnectionInfo::Credential(connection)) = connection else {
                    return event;
                };
                let Some(CredentialValue::Oauth(value)) = credential else {
                    return event;
                };
                if value.method_id.0 != METHOD_ID {
                    return event;
                }
                let account = normalize_account(account_metadata(value.metadata.as_ref()).and_then(Value::as_str));
                let refreshed = token(
                    http.as_ref(),
                    &user_agent,
                    &account,
                    &[("grant_type", "refresh_token"), ("refresh_token", value.refresh.as_str())],
                )
                .await;
                let refreshed = match refreshed {
                    Ok(refreshed) => refreshed,
                    Err(error) => {
                        log::warn!("Snowflake Cortex token refresh failed: {error}");
                        return event;
                    }
                };
                if let Err(error) = credentials
                    .update(&connection.id, Update { label: None, value: Some(CredentialValue::Oauth(refreshed)) })
                {
                    log::warn!("Snowflake Cortex credential update failed: {error}");
                    return event;
                }
                event.decision = RetryDecision::Retry { delay_ms: 0 };
                event
            })
        }
    }));
}

async fn begin_authorization(
    http: Arc<dyn HttpSender>,
    user_agent: String,
    answer: FormAnswer,
) -> Result<OAuthAuthorization, AuthorizationError> {
    let account = normalize_account(answer_string(&answer, "account").as_deref());
    if account.is_empty() {
        return Err(error("Snowflake account is required"));
    }
    let role = answer_string(&answer, "role").map(|role| role.trim().to_owned()).unwrap_or_default();
    let verifier = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 48]>());
    let challenge = pkce::challenge(&verifier);
    let state = pkce::state();
    let (sender, receiver) = oneshot::channel::<Result<String, String>>();
    let responses: Arc<CallbackResponses> = Arc::new(Mutex::new(Some(sender)));
    let handler_state = state.clone();
    let handler_responses = responses.clone();
    let server =
        CallbackServer::bind(CALLBACK_HOST, 0, move |request| handle(&handler_state, &handler_responses, request))
            .await
            .map_err(AuthorizationError::new)?;
    let redirect = format!("http://{CALLBACK_HOST}:{}/", server.port());
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", CLIENT_ID)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", &redirect)
        .append_pair("state", &state)
        .append_pair("scope", &scope(&role))
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .finish();
    let callback_account = account.clone();
    let callback: AutoCallback = Box::new(move || {
        let http = http.clone();
        let user_agent = user_agent.clone();
        let account = callback_account.clone();
        let redirect = redirect.clone();
        let verifier = verifier.clone();
        Box::pin(async move {
            let _server = server;
            let code = match receiver.await {
                Ok(Ok(code)) => code,
                Ok(Err(message)) => return Err(error(message)),
                Err(_) => return Err(error(AUTHORIZATION_FAILED)),
            };
            token(
                http.as_ref(),
                &user_agent,
                &account,
                &[
                    ("grant_type", "authorization_code"),
                    ("code", code.as_str()),
                    ("redirect_uri", redirect.as_str()),
                    ("code_verifier", verifier.as_str()),
                ],
            )
            .await
        })
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
        url: format!("{}/oauth/authorize?{query}", issuer(&account)),
        instructions: INSTRUCTIONS.to_owned(),
        expires_at: None,
        mode: OAuthMode::Auto { callback, submit: Some(submit) },
    })
}

fn handle(state: &str, responses: &CallbackResponses, request: Request) -> Reply {
    if request.path != "/" {
        return (404, "", String::new());
    }
    let params = parse_query(&request.query);
    let outcome = if param(&params, "state") != Some(state) {
        Err(INVALID_STATE.to_owned())
    } else if let Some(detail) = param(&params, "error_description").filter(|detail| !detail.is_empty()) {
        Err(detail.to_owned())
    } else if let Some(detail) = param(&params, "error").filter(|error| !error.is_empty()) {
        Err(detail.to_owned())
    } else if let Some(code) = param(&params, "code").filter(|code| !code.is_empty()) {
        Ok(code.to_owned())
    } else {
        Err(MISSING_CODE.to_owned())
    };
    let failure = outcome.as_ref().err().cloned();
    send(responses, outcome);
    match failure {
        Some(message) => (400, "text/html", page::error(&message, Some(page_options()))),
        None => (200, "text/html", page::success(Some(page_options()))),
    }
}

fn submit_text(state: &str, text: &str, responses: &CallbackResponses) -> Result<(), AuthorizationError> {
    match redirect::pasted(text, state).map_err(|failure| error(failure.to_string()))? {
        Pasted::Code { code, .. } => {
            send(responses, Ok(code));
            Ok(())
        }
        Pasted::Error { error, .. } => {
            send(responses, Err(error));
            Ok(())
        }
    }
}

fn send(responses: &CallbackResponses, outcome: Result<String, String>) {
    if let Some(sender) = responses.lock().expect("lock the Snowflake authorization callback").take() {
        let _ = sender.send(outcome);
    }
}

async fn token(
    http: &dyn HttpSender,
    user_agent: &str,
    account: &str,
    fields: &[(&str, &str)],
) -> Result<CredentialOAuth, AuthorizationError> {
    if account.is_empty() {
        return Err(error("Snowflake account is required"));
    }
    let request_refresh = fields.iter().find(|(key, _)| *key == "refresh_token").map(|(_, value)| (*value).to_owned());
    let mut form = fields.to_vec();
    form.push(("client_id", CLIENT_ID));
    let headers = vec![
        ("accept".to_owned(), "application/json".to_owned()),
        ("user-agent".to_owned(), user_agent.to_owned()),
        ("authorization".to_owned(), format!("Basic {}", basic())),
    ];
    let request = http::form(format!("{}/oauth/token-request", issuer(account)), headers, &form);
    match http::send(http, request, None).await {
        Ok(response) => credential(&response.body, account, request_refresh),
        Err(HttpError::Status(response)) => Err(error(format!(
            "Snowflake token request failed ({}): {}",
            response.status,
            String::from_utf8_lossy(&response.body)
        ))),
        Err(failure) => Err(error(format!("Snowflake token request failed: {failure}"))),
    }
}

fn credential(
    body: &[u8],
    account: &str,
    request_refresh: Option<String>,
) -> Result<CredentialOAuth, AuthorizationError> {
    let tokens: Token = serde_json::from_slice(body).map_err(|_| error("Invalid Snowflake token response"))?;
    if tokens.access_token.is_empty() {
        return Err(error("Invalid Snowflake token response"));
    }
    let refresh = tokens
        .refresh_token
        .filter(|value| !value.is_empty())
        .or(request_refresh)
        .ok_or_else(|| error("Snowflake token response did not include refresh_token"))?;
    let expires = opencode_util::time::now_ms() as f64 + tokens.expires_in.unwrap_or(DEFAULT_EXPIRY_SECONDS) * 1000.0;
    Ok(CredentialOAuth {
        r#type: CredentialOAuthType::Oauth,
        method_id: IntegrationMethodID(METHOD_ID.to_owned()),
        refresh,
        access: tokens.access_token,
        expires,
        metadata: Some(JsonMap(vec![("account".to_owned(), Value::String(account.to_owned()))])),
    })
}

async fn load_account(
    integrations: &IntegrationService,
    credentials: &CredentialService,
    environment: &ConfigEnv,
    configured: Option<&ProviderSettings>,
    integration_id: &IntegrationID,
    account: &Mutex<String>,
) -> bool {
    let (_, credential) = settings::read_stored_credential(integrations, credentials, integration_id).await;
    let next = resolve_account(environment, configured, credential.as_ref());
    let mut current = account.lock().expect("lock the Snowflake account");
    if *current == next {
        return false;
    }
    *current = next;
    true
}

fn resolve_account(
    environment: &ConfigEnv,
    configured: Option<&ProviderSettings>,
    credential: Option<&CredentialValue>,
) -> String {
    if let Some(account) = (environment)("SNOWFLAKE_ACCOUNT") {
        let account = account.to_string_lossy();
        return normalize_account(Some(account.as_ref()));
    }
    let account = match credential.and_then(metadata_account) {
        Some(value) if !value.is_null() => value.as_str(),
        _ => setting(configured, "account"),
    };
    normalize_account(account)
}

fn metadata_account(credential: &CredentialValue) -> Option<&Value> {
    account_metadata(match credential {
        CredentialValue::Key(value) => value.metadata.as_ref(),
        CredentialValue::Oauth(value) => value.metadata.as_ref(),
        CredentialValue::External(value) => value.metadata.as_ref(),
    })
}

fn account_metadata(metadata: Option<&JsonMap<Value>>) -> Option<&Value> {
    metadata?.0.iter().find(|(key, _)| key == "account").map(|(_, value)| value)
}

fn normalize_account(value: Option<&str>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    let trimmed = SCHEME.replace(value.trim(), "");
    let trimmed = TRAILING_SLASHES.replace(&trimmed, "");
    TRAILING_DOMAIN.replace(&trimmed, "").into_owned()
}

fn issuer(account: &str) -> String {
    format!("https://{account}.snowflakecomputing.com")
}

fn endpoint(value: Option<&str>, account: &str) -> String {
    let base = value
        .map(str::to_owned)
        .unwrap_or_else(|| "https://${SNOWFLAKE_ACCOUNT}.snowflakecomputing.com/api/v2/cortex/v1".to_owned());
    if account.is_empty() {
        return base;
    }
    base.replace("${SNOWFLAKE_ACCOUNT}", account)
}

fn scope(role: &str) -> String {
    if role.is_empty() {
        return "refresh_token".to_owned();
    }
    if ROLE.is_match(role) {
        return format!("refresh_token session:role:{role}");
    }
    format!("refresh_token session:role-encoded:{}", utf8_percent_encode(role, ENCODE_URI_COMPONENT))
}

fn env_token(environment: &ConfigEnv) -> Option<String> {
    env_value(environment, "SNOWFLAKE_CORTEX_TOKEN").or_else(|| env_value(environment, "SNOWFLAKE_CORTEX_PAT"))
}

fn answer_string(answer: &FormAnswer, key: &str) -> Option<String> {
    answer.0.iter().find(|(name, _)| name == key).and_then(|(_, value)| match value {
        FormValue::String(value) => Some(value.clone()),
        _ => None,
    })
}

fn account_form() -> FormFields1 {
    FormFields1(vec![FormField1::String(FormStringField1 {
        key: "account".to_owned(),
        title: Some("Snowflake account".to_owned()),
        description: None,
        required: Some(true),
        hidden: None,
        when: None,
        r#type: FormStringField1Type::String,
        format: None,
        min_length: None,
        max_length: None,
        pattern: None,
        placeholder: Some("myorg-myaccount".to_owned()),
        default: None,
        options: None,
        custom: None,
    })])
}

fn oauth_form() -> FormFields1 {
    let mut fields = account_form().0;
    fields.push(FormField1::String(FormStringField1 {
        key: "role".to_owned(),
        title: Some("Snowflake role (optional)".to_owned()),
        description: None,
        required: None,
        hidden: None,
        when: None,
        r#type: FormStringField1Type::String,
        format: None,
        min_length: None,
        max_length: None,
        pattern: None,
        placeholder: None,
        default: None,
        options: None,
        custom: None,
    }));
    FormFields1(fields)
}

fn empty_compatibility() -> ModelCompatibility {
    ModelCompatibility {
        reasoning_field: None,
        require_reasoning: None,
        max_tokens_field: None,
        require_finish_reason: None,
        require_assistant_after_tool: None,
        supports_prompt_cache_key: None,
        stream_usage: None,
        cache_format: None,
        supports_thinking_block_binding: None,
        supports_effort_updates: None,
        media_input: None,
    }
}

fn error_message(body: &[u8]) -> String {
    let Ok(Value::Object(error)) = serde_json::from_slice::<Value>(body) else {
        return String::new();
    };
    let chosen = [error.get("message"), error.get("error")].into_iter().flatten().find(|value| truthy(value));
    chosen.map(coerce).unwrap_or_default()
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0 && !value.is_nan()),
        Value::String(value) => !value.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

fn coerce(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => number(value),
        Value::String(value) => value.clone(),
        Value::Array(values) => values.iter().map(coerce).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".to_owned(),
    }
}

fn number(value: &serde_json::Number) -> String {
    if let Some(integer) = value.as_i64() {
        return integer.to_string();
    }
    if let Some(unsigned) = value.as_u64() {
        return unsigned.to_string();
    }
    if let Some(float) = value.as_f64()
        && float.fract() == 0.0
        && float.abs() < 1e21
    {
        return format!("{}", float as i64);
    }
    value.to_string()
}

fn upsert(headers: &mut Vec<(String, String)>, name: &str, value: String) {
    headers.retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
    headers.push((name.to_owned(), value));
}

fn basic() -> String {
    STANDARD.encode(format!("{CLIENT_ID}:{CLIENT_ID}"))
}

fn parse_query(query: &str) -> Vec<(String, String)> {
    url::form_urlencoded::parse(query.as_bytes()).map(|(key, value)| (key.into_owned(), value.into_owned())).collect()
}

fn param<'a>(params: &'a [(String, String)], name: &str) -> Option<&'a str> {
    params.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
}

fn page_options() -> CallbackPageOptions {
    CallbackPageOptions { provider: Some("Snowflake".to_owned()), auto_close: None }
}

fn error(message: impl Into<String>) -> AuthorizationError {
    AuthorizationError::new(io::Error::other(message.into()))
}
