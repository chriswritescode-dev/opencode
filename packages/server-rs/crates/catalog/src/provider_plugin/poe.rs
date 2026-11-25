use std::io;
use std::sync::{Arc, Mutex};

use opencode_ai::HttpSender;
use opencode_contract::generated::{
    CredentialOAuth, CredentialOAuthType, FormAnswer, IntegrationID, IntegrationMethodID, IntegrationOAuthMethod,
    IntegrationOAuthMethodType,
};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::oneshot;

use crate::builtin::{Builtin, Services};
use crate::integration::{
    AuthorizationError, AutoCallback, Implementation, OAuthAuthorization, OAuthFuture, OAuthImplementation, OAuthMode,
    SubmitCallback,
};
use crate::oauth::callback::{CallbackServer, HandlerFuture, Reply, Request};
use crate::oauth::page::{self, CallbackPageOptions};
use crate::oauth::pkce;
use crate::oauth::redirect::{self, Pasted};
use crate::provider_plugin::http::{self, HttpError};

pub const ID: &str = "opencode.provider.poe";

const INTEGRATION_ID: &str = "poe";

const METHOD_ID: &str = "browser";

const CLIENT_ID: &str = "client_728290227fc048cc9262091a1ea197ea";

const ISSUER: &str = "https://poe.com";

const TOKEN_ENDPOINT: &str = "https://api.poe.com/token";

const SCOPE: &str = "apikey:create";

const INSTRUCTIONS: &str = "Complete authorization in your browser. This window will close automatically.";

const CALLBACK_HOST: &str = "127.0.0.1";

const CALLBACK_PATH: &str = "/callback";

const MAX_EXPIRY: f64 = 8_640_000_000_000_000.0;

const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

const REQUEST_FAILED: &str = "Poe token exchange request failed";

const INVALID_RESPONSE: &str = "Invalid Poe token response";

const INVALID_EXPIRY: &str = "Invalid Poe API key expiry";

const EXPIRED: &str = "Poe API key expired. Log in with Poe again.";

struct PoeOAuth {
    integration_id: IntegrationID,
    method: IntegrationOAuthMethod,
    http: Arc<dyn HttpSender>,
}

impl OAuthImplementation for PoeOAuth {
    fn integration_id(&self) -> &IntegrationID {
        &self.integration_id
    }

    fn method(&self) -> &IntegrationOAuthMethod {
        &self.method
    }

    fn authorize(&self, _answer: FormAnswer) -> OAuthFuture<OAuthAuthorization> {
        Box::pin(begin_authorization(self.http.clone()))
    }

    fn refresh(&self) -> Option<crate::integration::RefreshCallback> {
        Some(Box::new(|value: CredentialOAuth| {
            Box::pin(async move {
                if value.expires > opencode_util::time::now_ms() as f64 {
                    return Ok(value);
                }
                Err(error(EXPIRED))
            })
        }))
    }
}

struct CallbackMessage {
    code: String,
    respond: Option<oneshot::Sender<Reply>>,
}

type CallbackSender = oneshot::Sender<Result<CallbackMessage, String>>;

type CallbackResponses = Mutex<Option<CallbackSender>>;

pub async fn register(services: &Services, context: &crate::provider_plugin::PluginContext, builtin: &mut Builtin) {
    let registration = services
        .integration
        .transform({
            let http = context.http.clone();
            move |editor| {
                editor.method_update(Implementation::OAuth(Arc::new(PoeOAuth {
                    integration_id: IntegrationID(INTEGRATION_ID.to_owned()),
                    method: IntegrationOAuthMethod {
                        id: IntegrationMethodID(METHOD_ID.to_owned()),
                        r#type: IntegrationOAuthMethodType::Oauth,
                        label: "Login with Poe (browser)".to_owned(),
                        form: None,
                    },
                    http: http.clone(),
                })));
                Ok(())
            }
        })
        .await;
    builtin.registrations.push(registration);
}

async fn begin_authorization(http: Arc<dyn HttpSender>) -> Result<OAuthAuthorization, AuthorizationError> {
    let pkce::Pkce { verifier, challenge } = pkce::generate();
    let state = pkce::state();
    let (sender, receiver) = oneshot::channel::<Result<CallbackMessage, String>>();
    let responses: Arc<CallbackResponses> = Arc::new(Mutex::new(Some(sender)));
    let server = CallbackServer::bind_async(CALLBACK_HOST, 0, {
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
    let redirect = format!("http://{CALLBACK_HOST}:{}{}", server.port(), CALLBACK_PATH);
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("response_type", "code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", &redirect)
        .append_pair("scope", SCOPE)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state)
        .finish();
    let callback: AutoCallback = Box::new(move || {
        let http = http.clone();
        let verifier = verifier.clone();
        let redirect = redirect.clone();
        Box::pin(async move {
            let _server = server;
            let message = match receiver.await {
                Ok(Ok(message)) => message,
                Ok(Err(message)) => return Err(error(message)),
                Err(_) => return Err(error("Authorization failed")),
            };
            let result = exchange(http.as_ref(), &message.code, &redirect, &verifier).await;
            if let Some(respond) = message.respond {
                let reply = match &result {
                    Ok(_) => (200, "text/html", page::success(Some(page_options()))),
                    Err(failure) => (400, "text/html", page::error(&failure.message(), Some(page_options()))),
                };
                let _ = respond.send(reply);
            }
            result
        })
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
        url: format!("{ISSUER}/oauth/authorize?{query}"),
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
    if param(&params, "state") != Some(state) {
        return Some((400, "text/html", page::error("Invalid OAuth state", Some(page_options()))));
    }
    if let Some(message) = callback_error(&params, state) {
        send(responses, Err(message.clone()));
        return Some((400, "text/html", page::error(&message, Some(page_options()))));
    }
    let code = param(&params, "code").map(str::trim).unwrap_or_default().to_owned();
    let Some(sender) = responses.lock().expect("lock the Poe authorization callback").take() else {
        return Some((409, "", "OAuth callback already received".to_owned()));
    };
    let (respond, reply) = oneshot::channel();
    let _ = sender.send(Ok(CallbackMessage { code, respond: Some(respond) }));
    reply.await.ok()
}

fn submit_text(state: &str, text: &str, responses: &CallbackResponses) -> Result<(), AuthorizationError> {
    let pasted = redirect::pasted(text, state).map_err(|failure| error(failure.to_string()))?;
    let params = match &pasted {
        Pasted::Code { params, .. } | Pasted::Error { params, .. } => params,
    };
    if let Some(message) = callback_error(params, state) {
        send(responses, Err(message));
        return Ok(());
    }
    let Pasted::Code { code, .. } = pasted else {
        send(responses, Err("Missing authorization code".to_owned()));
        return Ok(());
    };
    send(responses, Ok(CallbackMessage { code, respond: None }));
    Ok(())
}

fn send(responses: &CallbackResponses, outcome: Result<CallbackMessage, String>) {
    if let Some(sender) = responses.lock().expect("lock the Poe authorization callback").take() {
        let _ = sender.send(outcome);
    }
}

async fn exchange(
    http: &dyn HttpSender,
    code: &str,
    redirect: &str,
    verifier: &str,
) -> Result<CredentialOAuth, AuthorizationError> {
    let request = http::form(
        TOKEN_ENDPOINT,
        Vec::new(),
        &[
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("code", code),
            ("redirect_uri", redirect),
            ("code_verifier", verifier),
        ],
    );
    match http::send(http, request, None).await {
        Ok(response) => parse_token(&response.body),
        Err(HttpError::Status(response)) => {
            Err(error(exchange_failure(response.status, &response.body, code, verifier)))
        }
        Err(_) => Err(error(REQUEST_FAILED)),
    }
}

fn parse_token(body: &[u8]) -> Result<CredentialOAuth, AuthorizationError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| error(INVALID_RESPONSE))?;
    let api_key = value
        .get("api_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|key| !key.is_empty() && !key.chars().any(char::is_whitespace))
        .ok_or_else(|| error(INVALID_RESPONSE))?;
    let seconds = match value.get("api_key_expires_in") {
        None | Some(Value::Null) => None,
        Some(Value::Number(number)) => Some(
            number
                .as_f64()
                .filter(|seconds| seconds.is_finite() && *seconds >= 0.0 && seconds.fract() == 0.0)
                .ok_or_else(|| error(INVALID_RESPONSE))?,
        ),
        Some(_) => return Err(error(INVALID_RESPONSE)),
    };
    let expires = match seconds {
        None => MAX_EXPIRY,
        Some(seconds) => opencode_util::time::now_ms() as f64 + seconds * 1000.0,
    };
    if !(expires.is_finite() && expires.fract() == 0.0 && expires.abs() <= MAX_SAFE_INTEGER) || expires > MAX_EXPIRY {
        return Err(error(INVALID_EXPIRY));
    }
    Ok(CredentialOAuth {
        r#type: CredentialOAuthType::Oauth,
        method_id: IntegrationMethodID(METHOD_ID.to_owned()),
        refresh: String::new(),
        access: api_key.to_owned(),
        expires,
        metadata: None,
    })
}

fn exchange_failure(status: u16, body: &[u8], code: &str, verifier: &str) -> String {
    let detail = serde_json::from_slice::<Failure>(body).ok().and_then(|failure| {
        failure
            .error_description
            .filter(|detail| !detail.is_empty())
            .or(failure.error.filter(|detail| !detail.is_empty()))
    });
    match detail {
        Some(detail) if !detail.contains(code) && !detail.contains(verifier) => {
            format!("Poe token exchange failed: {detail}")
        }
        _ => format!("Poe token exchange failed ({status})"),
    }
}

#[derive(Deserialize)]
struct Failure {
    error: Option<String>,
    error_description: Option<String>,
}

fn callback_error(params: &[(String, String)], state: &str) -> Option<String> {
    if param(params, "state") != Some(state) {
        return Some("Invalid OAuth state".to_owned());
    }
    if param(params, "iss").is_some_and(|issuer| issuer != ISSUER) {
        return Some("Invalid OAuth issuer".to_owned());
    }
    if param(params, "error").is_some() {
        let detail = param(params, "error_description")
            .filter(|detail| !detail.is_empty())
            .or_else(|| param(params, "error"))
            .unwrap_or("Authorization denied");
        return Some(if detail.contains(state) { "Poe authorization failed".to_owned() } else { detail.to_owned() });
    }
    match param(params, "code").map(str::trim).filter(|code| !code.is_empty()) {
        Some(_) => None,
        None => Some("Missing authorization code".to_owned()),
    }
}

fn parse_query(query: &str) -> Vec<(String, String)> {
    url::form_urlencoded::parse(query.as_bytes()).map(|(key, value)| (key.into_owned(), value.into_owned())).collect()
}

fn param<'a>(params: &'a [(String, String)], name: &str) -> Option<&'a str> {
    params.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
}

fn page_options() -> CallbackPageOptions {
    CallbackPageOptions { provider: Some("Poe".to_owned()), auto_close: None }
}

fn error(message: impl Into<String>) -> AuthorizationError {
    AuthorizationError::new(io::Error::other(message.into()))
}
