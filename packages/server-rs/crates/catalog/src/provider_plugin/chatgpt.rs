use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use opencode_ai::HttpSender;
use opencode_contract::codec::{JsonMap, encode_json};
use opencode_contract::generated::{
    ConnectionInfo, CredentialOAuth, CredentialOAuthType, CredentialValue, FormAnswer, IntegrationID,
    IntegrationMethodID, IntegrationOAuthMethod, IntegrationOAuthMethodType, ModelCompatibility, ModelSettings,
    ProviderCompaction, ProviderCompactionSummary, ProviderCompactionSummaryType, ProviderEncoding, ProviderID,
    ProviderSettings, ProviderTransport,
};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::oneshot;

use crate::builtin::{Builtin, Services};
use crate::credential::{CredentialService, Update};
use crate::integration::{
    AuthorizationError, AutoCallback, Implementation, IntegrationService, OAuthAuthorization, OAuthFuture,
    OAuthImplementation, OAuthMode, RefreshCallback, RevokeCallback, SubmitCallback,
};
use crate::oauth::callback::{CallbackServer, HandlerFuture, Reply, Request};
use crate::oauth::page::{self, CallbackPageOptions};
use crate::oauth::pkce;
use crate::oauth::redirect::{self, Pasted};
use crate::provider;
use crate::provider_plugin::hooks::{self, RetryDecision};
use crate::provider_plugin::http::{self, HttpError};
use crate::provider_plugin::storage::Storage;
use crate::provider_plugin::{PluginContext, refresh, setting, settings};

pub const ID: &str = "opencode.provider.chatgpt";

const INTEGRATION_ID: &str = "openai";

const PROVIDER_ID: &str = "openai";

const METHOD_ID: &str = "chatgpt-token-sharing";

const LEGACY_METHOD_ID: &str = "chatgpt-browser";

const LABEL: &str = "Sign in with ChatGPT";

const ISSUER: &str = "https://auth.openai.com";

const TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";

const REVOKE_URL: &str = "https://auth.openai.com/api/accounts/oauth/revoke";

const JWKS_URL: &str = "https://auth.openai.com/.well-known/jwks.json";

const RESOURCE: &str = "https://api.openai.com/v1";

const REGISTRATION_CLIENT_ID: &str = "dynamic_agent_client";

const AGENT_NAME: &str = "OpenCode";

const TOKEN_SHARING_SCOPE: &str = "chatgpt.tokens.use.direct";

const SCOPE: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";

const INSTRUCTIONS: &str = "Complete authorization in your browser. This window will close automatically.";

const CALLBACK_HOST: &str = "127.0.0.1";

const CALLBACK_PORT: u16 = 0;

const CALLBACK_PATH: &str = "/auth/callback";

const HOST_ID_KEY: &str = "chatgpt-agent-host-id";

const DEFAULT_EXPIRES_SECONDS: f64 = 3600.0;

const REVOKE_TIMEOUT: Duration = Duration::from_secs(5);

const TOKEN_TIMEOUT: Duration = Duration::from_secs(10);

const JWKS_TIMEOUT: Duration = Duration::from_secs(5);

const FALLBACK_MODELS: [&str; 22] = [
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
    "gpt-6.2-astra",
    "gpt-6.2-astra-fast",
    "gpt-6.2-sol",
    "gpt-6.2-sol-fast",
];

const NON_RETRYABLE_SHARING_CODES: [&str; 8] = [
    "subscription_sharing_usage_limit_exceeded",
    "subscription_sharing_workplace_individual_usage_limit_exceeded",
    "subscription_sharing_user_not_eligible",
    "subscription_sharing_unsupported_capability",
    "subscription_sharing_route_not_supported",
    "subscription_sharing_invalid_user",
    "chatpass_v2_scope_not_authorized",
    "chatpass_v2_invalid_authorization_context",
];

const MISSING_CLIENT_ID: &str = "This ChatGPT connection has no registered client ID.";

const MISSING_CLIENT_ID_CONNECT: &str = "This ChatGPT connection has no registered client ID. Connect again.";

const DIFFERENT_CLIENT: &str = "ChatGPT returned a different client than this connection. Connect again.";

const MISSING_RETURNED_CLIENT: &str = "ChatGPT sign-in did not return a client ID. Connect again.";

const NO_TOKEN_SHARING: &str = "ChatGPT sign-in finished without token sharing. Sign in again and allow token sharing, or connect OpenAI with an API key.";

const MISSING_ID_TOKEN: &str = "ChatGPT sign-in did not return an ID token.";

const INVALID_ID_TOKEN: &str = "ChatGPT sign-in returned an invalid ID token.";

const MISSING_CODE: &str = "Missing authorization code";

const INVALID_STATE: &str = "Invalid OAuth state";

const AUTHORIZATION_FAILED: &str = "Authorization failed";

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    id_token: Option<String>,
    expires_in: Option<f64>,
    scope: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Metadata {
    #[serde(rename = "clientID")]
    client_id: String,
    scopes: Option<Vec<String>>,
    models: Option<Value>,
}

struct ChatGPTOAuth {
    integration_id: IntegrationID,
    method: IntegrationOAuthMethod,
    http: Arc<dyn HttpSender>,
    user_agent: String,
    chatgpt: Arc<Mutex<Option<CredentialOAuth>>>,
    storage: Arc<Storage>,
}

impl OAuthImplementation for ChatGPTOAuth {
    fn integration_id(&self) -> &IntegrationID {
        &self.integration_id
    }

    fn method(&self) -> &IntegrationOAuthMethod {
        &self.method
    }

    fn authorize(&self, _answer: FormAnswer) -> OAuthFuture<OAuthAuthorization> {
        Box::pin(begin_authorization(
            self.http.clone(),
            self.user_agent.clone(),
            self.chatgpt.clone(),
            self.storage.clone(),
        ))
    }

    fn refresh(&self) -> Option<RefreshCallback> {
        let http = self.http.clone();
        let user_agent = self.user_agent.clone();
        Some(Box::new(move |value: CredentialOAuth| {
            let http = http.clone();
            let user_agent = user_agent.clone();
            Box::pin(async move { refresh_credential(http.as_ref(), &user_agent, value).await })
        }))
    }

    fn revoke(&self) -> Option<RevokeCallback> {
        let http = self.http.clone();
        let user_agent = self.user_agent.clone();
        Some(Box::new(move |value: CredentialOAuth| {
            let http = http.clone();
            let user_agent = user_agent.clone();
            Box::pin(async move { revoke(http.as_ref(), &user_agent, REVOKE_URL, &value).await })
        }))
    }
}

#[derive(Clone)]
struct SignIn {
    redirect: String,
    verifier: String,
    nonce: String,
}

struct CallbackMessage {
    code: String,
    client_id: Option<String>,
    respond: Option<oneshot::Sender<Reply>>,
}

type CallbackSender = oneshot::Sender<Result<CallbackMessage, String>>;

type CallbackResponses = Mutex<Option<CallbackSender>>;

pub async fn register(services: &Services, context: &PluginContext, builtin: &mut Builtin) {
    let provider_id = ProviderID(PROVIDER_ID.to_owned());
    let integration_id = IntegrationID(INTEGRATION_ID.to_owned());
    let chatgpt: Arc<Mutex<Option<CredentialOAuth>>> = Arc::new(Mutex::new(None));
    let source: Arc<Mutex<Option<ConnectionInfo>>> = Arc::new(Mutex::new(None));
    let storage = Arc::new(Storage::new(context.kv.clone(), ID));

    let integration_registration = services
        .integration
        .transform({
            let http = context.http.clone();
            let user_agent = context.app.useragent();
            let chatgpt = chatgpt.clone();
            let storage = storage.clone();
            move |editor| {
                editor.method_update(Implementation::OAuth(Arc::new(ChatGPTOAuth {
                    integration_id: IntegrationID(INTEGRATION_ID.to_owned()),
                    method: IntegrationOAuthMethod {
                        id: IntegrationMethodID(METHOD_ID.to_owned()),
                        r#type: IntegrationOAuthMethodType::Oauth,
                        label: LABEL.to_owned(),
                        form: None,
                    },
                    http: http.clone(),
                    user_agent: user_agent.clone(),
                    chatgpt: chatgpt.clone(),
                    storage: storage.clone(),
                })));
                Ok(())
            }
        })
        .await;
    builtin.registrations.push(integration_registration);

    migrate(&services.credentials, &storage, &integration_id).await;

    load(&services.integration, &services.credentials, &integration_id, &chatgpt, &source).await;

    let hooks = context.hooks.clone();
    builtin.hooks.push(hooks.on_retry(Some(provider_id.clone()), {
        let chatgpt = chatgpt.clone();
        move |mut event: hooks::Retry| {
            let chatgpt = chatgpt.clone();
            Box::pin(async move {
                if chatgpt.lock().expect("lock the ChatGPT credential").is_some()
                    && has_non_retryable_sharing_code(&event.error)
                {
                    event.decision = RetryDecision::Stop;
                }
                event
            })
        }
    }));
    builtin.hooks.push(hooks.on_model_request(Some(provider_id.clone()), {
        let chatgpt = chatgpt.clone();
        let sessions = context.sessions.clone();
        move |mut event| {
            let chatgpt = chatgpt.clone();
            let sessions = sessions.clone();
            Box::pin(async move {
                if chatgpt.lock().expect("lock the ChatGPT credential").is_none() {
                    return event;
                }
                let affinity = match sessions.session(event.session_id.clone()).await {
                    Some(session) => session.affinity().0,
                    None => event.session_id.0.clone(),
                };
                event.headers.insert("session-id".to_owned(), affinity);
                event.headers.insert("thread-id".to_owned(), event.session_id.0.clone());
                event.headers.insert("x-client-request-id".to_owned(), event.session_id.0.clone());
                event
            })
        }
    }));

    let configured = settings::configured_settings(services, &provider_id).await.ok().flatten();

    let provider_registration = services
        .provider
        .transform({
            let chatgpt = chatgpt.clone();
            let source = source.clone();
            let provider_id = provider_id.clone();
            let configured = configured.clone();
            move |editor| {
                let Some(item) = editor.get(&provider_id).cloned() else {
                    return Ok(());
                };
                let active = chatgpt.lock().expect("lock the ChatGPT credential").is_some();
                let source = source.lock().expect("lock the ChatGPT source connection").clone();
                let Some(source) = source.filter(|_| active) else {
                    return Ok(());
                };
                editor.update(&provider_id, |provider| {
                    let encoding = match setting(configured.as_ref(), "baseURL") {
                        None => Some(
                            provider
                                .settings
                                .as_ref()
                                .and_then(|settings| settings.encoding)
                                .unwrap_or(ProviderEncoding::Zstd),
                        ),
                        Some(_) => None,
                    };
                    let overlay = ProviderSettings {
                        timeout: None,
                        header_timeout: None,
                        chunk_timeout: None,
                        compaction: Some(ProviderCompaction::Summary(ProviderCompactionSummary {
                            r#type: ProviderCompactionSummaryType::Summary,
                        })),
                        cache: None,
                        transport: Some(ProviderTransport::Http),
                        encoding,
                        tls: None,
                        rest: JsonMap(vec![("baseURL".to_owned(), Value::String(RESOURCE.to_owned()))]),
                    };
                    let base = provider.settings.as_ref().map(provider::json_object);
                    let overlay = provider::json_object(&overlay);
                    provider.settings =
                        crate::aisdk_native::merge_overlay(base.as_ref(), Some(&overlay)).map(provider::decode_object);
                });
                let Some(updated) = editor.get(&provider_id).cloned() else {
                    return Ok(());
                };
                let models: Vec<_> = item
                    .models
                    .values()
                    .filter(|model| FALLBACK_MODELS.contains(&model.id.as_str()))
                    .cloned()
                    .collect();
                editor.add(updated.provider.clone(), &models, Some(source));
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
                if chatgpt.lock().expect("lock the ChatGPT credential").is_none() {
                    return Ok(());
                }
                for model in editor.list(Some(&provider_id)) {
                    let compatibility = model.compatibility.get_or_insert(ModelCompatibility {
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
                    });
                    compatibility.supports_effort_updates = Some(false);
                    let settings = model.settings.get_or_insert(ModelSettings {
                        compaction: None,
                        cache: None,
                        encoding: None,
                        rest: JsonMap(Vec::new()),
                    });
                    settings.compaction = Some(ProviderCompaction::Summary(ProviderCompactionSummary {
                        r#type: ProviderCompactionSummaryType::Summary,
                    }));
                    if reasoning_mode_is_pro(model.body.as_ref()) {
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

    let switch = refresh::on_switch(&services.bus, INTEGRATION_ID, {
        let integrations = services.integration.clone();
        let credentials = services.credentials.clone();
        let providers = services.provider.clone();
        let chatgpt = chatgpt.clone();
        let source = source.clone();
        let integration_id = integration_id.clone();
        move || {
            let integrations = integrations.clone();
            let credentials = credentials.clone();
            let providers = providers.clone();
            let chatgpt = chatgpt.clone();
            let source = source.clone();
            let integration_id = integration_id.clone();
            Box::pin(async move {
                if load(&integrations, &credentials, &integration_id, &chatgpt, &source).await {
                    providers.reload().await;
                }
            })
        }
    });
    builtin.tasks.push(switch);
}

async fn migrate(credentials: &CredentialService, storage: &Storage, integration_id: &IntegrationID) {
    let Ok(entries) = credentials.list(integration_id) else {
        return;
    };
    for entry in entries {
        let CredentialValue::Oauth(value) = &entry.value else {
            continue;
        };
        if value.method_id.0 != LEGACY_METHOD_ID {
            continue;
        }
        let Some(metadata) = decode_metadata(value.metadata.as_ref()) else {
            continue;
        };
        let key = model_cache_key(&metadata.client_id);
        if storage.get(&key).await.is_none_or(|cached| cached.as_array().is_none())
            && let Some(models) = metadata.models
        {
            storage.set(&key, models).await;
        }
        let rest = value
            .metadata
            .as_ref()
            .map(|metadata| metadata.0.iter().filter(|(key, _)| key != "models").cloned().collect())
            .unwrap_or_default();
        let mut next = value.clone();
        next.method_id = IntegrationMethodID(METHOD_ID.to_owned());
        next.metadata = Some(JsonMap(rest));
        let _ = credentials.update(&entry.id, Update { label: None, value: Some(CredentialValue::Oauth(next)) });
    }
}

async fn load(
    integrations: &IntegrationService,
    credentials: &CredentialService,
    integration_id: &IntegrationID,
    chatgpt: &Mutex<Option<CredentialOAuth>>,
    source: &Mutex<Option<ConnectionInfo>>,
) -> bool {
    let (connection, credential) = settings::read_stored_credential(integrations, credentials, integration_id).await;
    let next = match credential {
        Some(CredentialValue::Oauth(value)) if value.method_id.0 == METHOD_ID => Some(value),
        _ => None,
    };
    let next_source = next.as_ref().and(connection);
    let changed = {
        let mut current = source.lock().expect("lock the ChatGPT source connection");
        let changed = *current != next_source;
        *current = next_source;
        changed
    };
    *chatgpt.lock().expect("lock the ChatGPT credential") = next;
    changed
}

async fn begin_authorization(
    http: Arc<dyn HttpSender>,
    user_agent: String,
    chatgpt: Arc<Mutex<Option<CredentialOAuth>>>,
    storage: Arc<Storage>,
) -> Result<OAuthAuthorization, AuthorizationError> {
    let host_id = match storage.get(HOST_ID_KEY).await.as_ref().and_then(Value::as_str) {
        Some(host_id) => host_id.to_owned(),
        None => {
            let host_id = format!("urn:uuid:{}", uuid_v4());
            storage.set(HOST_ID_KEY, Value::String(host_id.clone())).await;
            host_id
        }
    };
    let pkce::Pkce { verifier, challenge } = pkce::generate();
    let state = pkce::state();
    let nonce = pkce::state();
    let saved_id = {
        let current = chatgpt.lock().expect("lock the ChatGPT credential");
        current.as_ref().and_then(|value| decode_metadata(value.metadata.as_ref())).map(|metadata| metadata.client_id)
    };
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
    .map_err(AuthorizationError::new)?;
    let redirect = format!("http://{CALLBACK_HOST}:{}{CALLBACK_PATH}", server.port());
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("client_id", saved_id.as_deref().unwrap_or(REGISTRATION_CLIENT_ID));
    if saved_id.is_none() {
        query.append_pair("agent_name_hint", AGENT_NAME);
    }
    query
        .append_pair("ext_agent_host_id", &host_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", &redirect)
        .append_pair("scope", SCOPE)
        .append_pair("resource", RESOURCE)
        .append_pair("state", &state)
        .append_pair("nonce", &nonce)
        .append_pair("code_challenge_method", "S256")
        .append_pair("code_challenge", &challenge);
    let query = query.finish();
    let sign_in = SignIn { redirect, verifier, nonce };
    let callback: AutoCallback = Box::new({
        let http = http.clone();
        let user_agent = user_agent.clone();
        let sign_in = sign_in.clone();
        let saved_id = saved_id.clone();
        move || {
            let http = http.clone();
            let user_agent = user_agent.clone();
            let sign_in = sign_in.clone();
            let saved_id = saved_id.clone();
            Box::pin(async move {
                let _server = server;
                let message = match receiver.await {
                    Ok(Ok(message)) => message,
                    Ok(Err(message)) => return Err(error(message)),
                    Err(_) => return Err(error(AUTHORIZATION_FAILED)),
                };
                let CallbackMessage { code, client_id, respond } = message;
                let result =
                    finish(http.as_ref(), &user_agent, &code, client_id.as_deref(), saved_id.as_deref(), &sign_in)
                        .await;
                if let Some(respond) = respond {
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
        move |pasted: String| {
            let responses = responses.clone();
            let state = state.clone();
            Box::pin(async move { submit_text(&state, &pasted, &responses) })
        }
    });
    Ok(OAuthAuthorization {
        url: format!("{ISSUER}/api/accounts/authorize?{query}"),
        instructions: INSTRUCTIONS.to_owned(),
        expires_at: None,
        mode: OAuthMode::Auto { callback, submit: Some(submit) },
    })
}

async fn handle(state: &str, responses: &CallbackResponses, request: Request) -> Option<Reply> {
    if request.method != "GET" || request.path != CALLBACK_PATH {
        return Some((404, "", "Not found".to_owned()));
    }
    let params = parse_query(&request.query);
    if param(&params, "state") != Some(state) {
        return Some((400, "text/html", page::error(INVALID_STATE, Some(page_options()))));
    }
    let failure =
        param(&params, "error_description").or_else(|| param(&params, "error")).filter(|detail| !detail.is_empty());
    if let Some(failure) = failure {
        send(responses, Err(failure.to_owned()));
        return Some((400, "text/html", page::error(failure, Some(page_options()))));
    }
    let Some(code) = param(&params, "code").filter(|code| !code.is_empty()) else {
        send(responses, Err(MISSING_CODE.to_owned()));
        return Some((400, "text/html", page::error(MISSING_CODE, Some(page_options()))));
    };
    let Some(sender) = responses.lock().expect("lock the ChatGPT authorization callback").take() else {
        return Some((409, "", "OAuth callback already received".to_owned()));
    };
    let client_id = param(&params, "client_id").map(str::to_owned);
    let (respond, reply) = oneshot::channel();
    let _ = sender.send(Ok(CallbackMessage { code: code.to_owned(), client_id, respond: Some(respond) }));
    reply.await.ok()
}

fn submit_text(state: &str, text: &str, responses: &CallbackResponses) -> Result<(), AuthorizationError> {
    let pasted = redirect::pasted(text, state).map_err(|failure| error(failure.to_string()))?;
    match pasted {
        Pasted::Code { code, params } => {
            let client_id = param(&params, "client_id").map(str::to_owned);
            send(responses, Ok(CallbackMessage { code, client_id, respond: None }));
        }
        Pasted::Error { error: message, .. } => send(responses, Err(message)),
    }
    Ok(())
}

fn send(responses: &CallbackResponses, outcome: Result<CallbackMessage, String>) {
    if let Some(sender) = responses.lock().expect("lock the ChatGPT authorization callback").take() {
        let _ = sender.send(outcome);
    }
}

async fn finish(
    http: &dyn HttpSender,
    user_agent: &str,
    code: &str,
    returned_id: Option<&str>,
    saved_id: Option<&str>,
    sign_in: &SignIn,
) -> Result<CredentialOAuth, AuthorizationError> {
    let returned_id = returned_id.filter(|client_id| !client_id.is_empty());
    if let (Some(saved), Some(returned)) = (saved_id, returned_id)
        && saved != returned
    {
        return Err(error(DIFFERENT_CLIENT));
    }
    let Some(client_id) = saved_id.or(returned_id) else {
        return Err(error(MISSING_RETURNED_CLIENT));
    };
    let tokens = exchange(http, user_agent, code, client_id, &sign_in.redirect, &sign_in.verifier).await?;
    if !tokens.scope.as_deref().is_some_and(|scope| scope.split(' ').any(|part| part == TOKEN_SHARING_SCOPE)) {
        return Err(error(NO_TOKEN_SHARING));
    }
    let id_token = tokens.id_token.clone().ok_or_else(|| error(MISSING_ID_TOKEN))?;
    verify_id_token(http, &id_token, client_id, &sign_in.nonce, JWKS_URL).await?;
    Ok(credential(&tokens, client_id, None))
}

async fn exchange(
    http: &dyn HttpSender,
    user_agent: &str,
    code: &str,
    client_id: &str,
    redirect: &str,
    verifier: &str,
) -> Result<TokenResponse, AuthorizationError> {
    request(
        http,
        user_agent,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", redirect),
            ("resource", RESOURCE),
        ],
    )
    .await
}

async fn refresh_credential(
    http: &dyn HttpSender,
    user_agent: &str,
    value: CredentialOAuth,
) -> Result<CredentialOAuth, AuthorizationError> {
    let Some(metadata) = decode_metadata(value.metadata.as_ref()) else {
        return Err(error(MISSING_CLIENT_ID_CONNECT));
    };
    let tokens = request(
        http,
        user_agent,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", &metadata.client_id),
            ("refresh_token", &value.refresh),
            ("resource", RESOURCE),
        ],
    )
    .await?;
    Ok(credential(&tokens, &metadata.client_id, metadata.scopes))
}

pub async fn revoke(
    http: &dyn HttpSender,
    user_agent: &str,
    url: &str,
    value: &CredentialOAuth,
) -> Result<(), AuthorizationError> {
    let Some(metadata) = decode_metadata(value.metadata.as_ref()) else {
        return Err(error(MISSING_CLIENT_ID));
    };
    let request = http::form(
        url,
        headers(user_agent),
        &[("token", &value.refresh), ("token_type_hint", "refresh_token"), ("client_id", &metadata.client_id)],
    );
    match http::send(http, request, Some(REVOKE_TIMEOUT)).await {
        Ok(_) => Ok(()),
        Err(HttpError::Status(response)) => Err(error(format!("Revocation failed: {}", response.status))),
        Err(failure) => Err(error(failure.to_string())),
    }
}

pub async fn verify_id_token(
    http: &dyn HttpSender,
    token: &str,
    client_id: &str,
    nonce: &str,
    jwks_url: &str,
) -> Result<(), AuthorizationError> {
    match check_id_token(http, token, client_id, nonce, jwks_url).await {
        Some(()) => Ok(()),
        None => Err(error(INVALID_ID_TOKEN)),
    }
}

async fn check_id_token(
    http: &dyn HttpSender,
    token: &str,
    client_id: &str,
    nonce: &str,
    jwks_url: &str,
) -> Option<()> {
    let mut parts = token.split('.');
    let header_segment = parts.next()?;
    let payload = parts.next()?;
    let signature = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let header: Value = serde_json::from_slice(&decode_base64(header_segment)?).ok()?;
    if header.get("alg")?.as_str()? != "RS256" {
        return None;
    }
    let kid = header.get("kid").and_then(Value::as_str);
    let claims: Value = serde_json::from_slice(&decode_base64(payload)?).ok()?;
    if claims.get("iss")?.as_str()? != ISSUER {
        return None;
    }
    let audience = claims.get("aud")?;
    let audience_matches = match audience {
        Value::String(audience) => audience == client_id,
        Value::Array(audience) => audience.iter().any(|value| value.as_str() == Some(client_id)),
        _ => false,
    };
    if !audience_matches {
        return None;
    }
    if claims.get("exp")?.as_f64()? * 1000.0 <= opencode_util::time::now_ms() as f64 {
        return None;
    }
    if claims.get("nonce")?.as_str()? != nonce {
        return None;
    }
    if claims.get("sub")?.as_str()?.trim().is_empty() {
        return None;
    }
    let signature = decode_base64(signature)?;
    let response = http::send(http, http::get(jwks_url, Vec::new()), Some(JWKS_TIMEOUT)).await.ok()?;
    let keys: Value = serde_json::from_slice(&response.body).ok()?;
    let keys = keys.get("keys")?.as_array()?;
    let message = format!("{header_segment}.{payload}");
    keys.iter().any(|key| verify_signature(key, kid, message.as_bytes(), &signature)).then_some(())
}

fn verify_signature(key: &Value, kid: Option<&str>, message: &[u8], signature: &[u8]) -> bool {
    if key.get("kty").and_then(Value::as_str) != Some("RSA") {
        return false;
    }
    if let Some(kid) = kid
        && key.get("kid").and_then(Value::as_str) != Some(kid)
    {
        return false;
    }
    let Some(n) = key.get("n").and_then(Value::as_str).and_then(decode_base64) else {
        return false;
    };
    let Some(e) = key.get("e").and_then(Value::as_str).and_then(decode_base64) else {
        return false;
    };
    ring::signature::RsaPublicKeyComponents { n: &n, e: &e }
        .verify(&ring::signature::RSA_PKCS1_2048_8192_SHA256, message, signature)
        .is_ok()
}

async fn request<T: serde::de::DeserializeOwned>(
    http: &dyn HttpSender,
    user_agent: &str,
    fields: &[(&str, &str)],
) -> Result<T, AuthorizationError> {
    match http::send(http, http::form(TOKEN_URL, headers(user_agent), fields), Some(TOKEN_TIMEOUT)).await {
        Ok(response) => {
            serde_json::from_slice(&response.body).map_err(|failure| error(format!("Request failed: {failure}")))
        }
        Err(HttpError::Status(response)) => Err(error(format!("Request failed: {}", response.status))),
        Err(failure) => Err(error(format!("Request failed: {failure}"))),
    }
}

fn headers(user_agent: &str) -> Vec<(String, String)> {
    vec![
        ("content-type".to_owned(), "application/x-www-form-urlencoded".to_owned()),
        ("user-agent".to_owned(), user_agent.to_owned()),
    ]
}

fn credential(tokens: &TokenResponse, client_id: &str, scopes: Option<Vec<String>>) -> CredentialOAuth {
    let scopes = tokens
        .scope
        .as_deref()
        .map(|scope| scope.split(' ').filter(|part| !part.is_empty()).map(str::to_owned).collect())
        .or(scopes)
        .unwrap_or_default();
    let metadata = JsonMap(vec![
        ("clientID".to_owned(), Value::String(client_id.to_owned())),
        ("scopes".to_owned(), Value::Array(scopes.into_iter().map(Value::String).collect())),
    ]);
    CredentialOAuth {
        r#type: CredentialOAuthType::Oauth,
        method_id: IntegrationMethodID(METHOD_ID.to_owned()),
        refresh: tokens.refresh_token.clone(),
        access: tokens.access_token.clone(),
        expires: opencode_util::time::now_ms() as f64 + tokens.expires_in.unwrap_or(DEFAULT_EXPIRES_SECONDS) * 1000.0,
        metadata: Some(metadata),
    }
}

fn decode_metadata(metadata: Option<&JsonMap<Value>>) -> Option<Metadata> {
    serde_json::from_str(&encode_json(metadata?)).ok()
}

fn model_cache_key(client_id: &str) -> String {
    format!("models:{client_id}")
}

fn reasoning_mode_is_pro(body: Option<&JsonMap<Value>>) -> bool {
    body.and_then(|body| body.0.iter().find(|(key, _)| key == "reasoning"))
        .and_then(|(_, reasoning)| reasoning.get("mode"))
        .and_then(Value::as_str)
        == Some("pro")
}

fn uuid_v4() -> String {
    let mut bytes = rand::random::<[u8; 16]>();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

fn decode_base64(value: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(value).or_else(|_| URL_SAFE.decode(value)).ok()
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

fn has_non_retryable_sharing_code(error: &Value) -> bool {
    let Some(body) = error.get("response").and_then(|response| response.get("body")).and_then(Value::as_str) else {
        return false;
    };
    NON_RETRYABLE_SHARING_CODES.iter().any(|code| body.contains(code))
}

fn error(message: impl Into<String>) -> AuthorizationError {
    AuthorizationError::new(io::Error::other(message.into()))
}
