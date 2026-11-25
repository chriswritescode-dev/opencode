use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use opencode_ai::HttpSender;
use opencode_contract::generated::{
    CredentialOAuth, CredentialOAuthType, CredentialValue, FormAnswer, IntegrationID, IntegrationMethodID,
    IntegrationOAuthMethod, IntegrationOAuthMethodType, ModelCapabilities, ModelInfoLimit, ProviderID,
};
use serde::Deserialize;
use tokio::sync::oneshot;

use crate::builtin::{Builtin, Services};
use crate::integration::{
    AuthorizationError, AutoCallback, Implementation, OAuthAuthorization, OAuthFuture, OAuthImplementation, OAuthMode,
};
use crate::oauth::callback::{CallbackServer, Request};
use crate::oauth::page::{self, BootstrapOptions};
use crate::provider_plugin::http::{self, HttpError};
use crate::provider_plugin::{PluginContext, refresh, settings};

pub const ID: &str = "opencode.provider.digitalocean";

const PROVIDER_ID: &str = "digitalocean";

const METHOD_ID: &str = "browser";

const CLIENT_ID: &str = "b1a6c5158156caac821fd1b30253ca8acb52454a48fa744420e41889cb589f82";

const AUTHORIZE_ENDPOINT: &str = "https://cloud.digitalocean.com/v1/oauth/authorize";

const ROUTERS_ENDPOINT: &str = "https://api.digitalocean.com/v2/gen-ai/models/routers";

const SCOPE: &str = "genai:read inference:query";

const INSTRUCTIONS: &str = "Complete authorization in your browser. This window will close automatically.";

const CALLBACK_HOST: &str = "localhost";

const CALLBACK_PORT: u16 = 1456;

const CALLBACK_PATH: &str = "/auth/callback";

const TOKEN_PATH: &str = "/auth/token";

const PORT_IN_USE: &str = "DigitalOcean login needs local port 1456, but it is already in use. Stop the process using that port and try again.";

const DEFAULT_EXPIRES_SECONDS: i64 = 60 * 60 * 24 * 30;

const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    pub interval: Option<Duration>,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Discovery {
    key: Option<String>,
    routers: Vec<Router>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
struct Router {
    name: String,
}

#[derive(Deserialize)]
struct RoutersResponse {
    model_routers: Vec<Router>,
}

#[derive(Clone, Debug, thiserror::Error)]
enum LoadError {
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error("invalid DigitalOcean routers response: {0}")]
    Json(String),
    #[error("DigitalOcean connection has no OAuth credential")]
    MissingCredential,
}

impl crate::shared_cache::Transient for LoadError {
    fn is_transient(&self) -> bool {
        matches!(self, Self::Http(error) if error.transient())
    }
}

#[derive(Default, Deserialize)]
struct CallbackBody {
    access_token: Option<String>,
    expires_in: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

struct CallbackToken {
    access: String,
    expires: f64,
}

type CallbackSender = oneshot::Sender<Result<CallbackToken, String>>;

type CallbackResponses = Mutex<Option<CallbackSender>>;

struct DigitalOceanOAuth {
    integration_id: IntegrationID,
    method: IntegrationOAuthMethod,
}

impl OAuthImplementation for DigitalOceanOAuth {
    fn integration_id(&self) -> &IntegrationID {
        &self.integration_id
    }

    fn method(&self) -> &IntegrationOAuthMethod {
        &self.method
    }

    fn authorize(&self, _answer: FormAnswer) -> OAuthFuture<OAuthAuthorization> {
        Box::pin(begin_authorization())
    }
}

pub async fn register(services: &Services, context: &PluginContext, builtin: &mut Builtin) {
    register_with(services, context, builtin, Options::default()).await
}

pub async fn register_with(services: &Services, context: &PluginContext, builtin: &mut Builtin, options: Options) {
    let provider_id = ProviderID(PROVIDER_ID.to_owned());
    let integration_id = IntegrationID(PROVIDER_ID.to_owned());

    let integration_registration = services
        .integration
        .transform(|editor| {
            editor.method_update(Implementation::OAuth(Arc::new(DigitalOceanOAuth {
                integration_id: IntegrationID(PROVIDER_ID.to_owned()),
                method: IntegrationOAuthMethod {
                    id: IntegrationMethodID(METHOD_ID.to_owned()),
                    r#type: IntegrationOAuthMethodType::Oauth,
                    label: "Login with DigitalOcean".to_owned(),
                    form: None,
                },
            })));
            Ok(())
        })
        .await;
    builtin.registrations.push(integration_registration);

    let integrations = services.integration.clone();
    let http = context.http.clone();
    let cache = context.shared_cache.clone();
    let user_agent = context.app.useragent();
    let reload = services.provider.clone();
    let active_integrations = integrations.clone();
    let active_integration_id = integration_id.clone();
    let load_integration_id = integration_id.clone();

    let discovered = refresh::start(
        &services.bus,
        refresh::Start::<Discovery, LoadError> {
            integration_id: PROVIDER_ID.to_owned(),
            initial: Discovery::default(),
            interval: options.interval,
            active: Box::new(move || {
                let integrations = active_integrations.clone();
                let integration_id = active_integration_id.clone();
                Box::pin(async move { integrations.connection().active(&integration_id).await.ok().flatten() })
            }),
            load: Box::new(move |current, options| {
                let integrations = integrations.clone();
                let http = http.clone();
                let cache = cache.clone();
                let user_agent = user_agent.clone();
                let integration_id = load_integration_id.clone();
                Box::pin(async move {
                    let (connection, credential) = settings::active_credential(&integrations, &integration_id).await;
                    let Some(connection) = connection else {
                        return Ok(refresh::Loaded { connection: None, value: Discovery::default() });
                    };
                    let Some(CredentialValue::Oauth(credential)) = credential else {
                        return Ok(refresh::Loaded { connection: Some(connection), value: Discovery::default() });
                    };
                    if credential.method_id.0 != METHOD_ID {
                        return Ok(refresh::Loaded { connection: Some(connection), value: Discovery::default() });
                    }
                    let key = format!("digitalocean:{}", crate::integration::key(&connection));
                    if credential.expires != 0.0 && credential.expires <= opencode_util::time::now_ms() as f64 {
                        let value = if current.key.as_deref() == Some(key.as_str()) {
                            current
                        } else {
                            Discovery { key: Some(key), routers: Vec::new() }
                        };
                        return Ok(refresh::Loaded { connection: Some(connection), value });
                    }
                    let routers = cache
                        .get(&key, options.interval, options.force, {
                            let integrations = integrations.clone();
                            let http = http.clone();
                            let user_agent = user_agent.clone();
                            let connection = connection.clone();
                            move || {
                                let integrations = integrations.clone();
                                let http = http.clone();
                                let user_agent = user_agent.clone();
                                let connection = connection.clone();
                                async move {
                                    let resolved = integrations.connection().resolve(&connection).await.ok().flatten();
                                    let Some(CredentialValue::Oauth(latest)) = resolved else {
                                        return Err(LoadError::MissingCredential);
                                    };
                                    if latest.method_id.0 != METHOD_ID {
                                        return Err(LoadError::MissingCredential);
                                    }
                                    load_routers(http.as_ref(), &user_agent, &latest.access).await
                                }
                            }
                        })
                        .await
                        .unwrap_or_default();
                    Ok(refresh::Loaded { connection: Some(connection), value: Discovery { key: Some(key), routers } })
                })
            }),
            reload: Box::new(move || {
                let providers = reload.clone();
                Box::pin(async move { providers.reload().await })
            }),
        },
    );

    let registration = services
        .provider
        .transform(move |editor| {
            if editor.get(&provider_id).is_none() {
                return Ok(());
            }
            let Discovery { routers, .. } = discovered.current();
            for router in routers {
                let id = format!("router:{}", router.name);
                if editor.get(&provider_id).is_some_and(|item| item.models.contains_key(&id)) {
                    continue;
                }
                let name = router.name.clone();
                editor.models_update(&provider_id, &id, move |model| {
                    model.name = name.clone();
                    model.family = Some("digitalocean-inference-routers".to_owned());
                    model.capabilities = ModelCapabilities {
                        tools: true,
                        freeform: None,
                        input: vec!["text".to_owned()],
                        output: vec!["text".to_owned()],
                        temperature: None,
                    };
                    model.limit = ModelInfoLimit { context: 128_000.0, input: None, output: 8_192.0 };
                });
            }
            Ok(())
        })
        .await;
    builtin.registrations.push(registration);
}

async fn begin_authorization() -> Result<OAuthAuthorization, AuthorizationError> {
    let state = generate_state();
    let (sender, receiver) = oneshot::channel::<Result<CallbackToken, String>>();
    let responses: Arc<CallbackResponses> = Arc::new(Mutex::new(Some(sender)));
    let handler_state = state.clone();
    let server =
        CallbackServer::bind(CALLBACK_HOST, CALLBACK_PORT, move |request| handle(&handler_state, &responses, request))
            .await
            .map_err(bind_error)?;
    let redirect = format!("http://localhost:{}{}", server.port(), CALLBACK_PATH);
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("response_type", "token")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", &redirect)
        .append_pair("scope", SCOPE)
        .append_pair("state", &state)
        .finish();
    let callback: AutoCallback = Box::new(move || {
        Box::pin(async move {
            let outcome = receiver.await;
            drop(server);
            let token = match outcome {
                Ok(Ok(token)) => token,
                Ok(Err(message)) => return Err(AuthorizationError::new(io::Error::other(message))),
                Err(_) => return Err(AuthorizationError::new(io::Error::other("Authorization failed"))),
            };
            Ok(CredentialOAuth {
                r#type: CredentialOAuthType::Oauth,
                method_id: IntegrationMethodID(METHOD_ID.to_owned()),
                refresh: String::new(),
                access: token.access,
                expires: token.expires,
                metadata: None,
            })
        })
    });
    Ok(OAuthAuthorization {
        url: format!("{AUTHORIZE_ENDPOINT}?{query}"),
        instructions: INSTRUCTIONS.to_owned(),
        expires_at: None,
        mode: OAuthMode::Auto { callback, submit: None },
    })
}

fn handle(state: &str, responses: &CallbackResponses, request: Request) -> (u16, &'static str, String) {
    if request.method == "GET" && request.path == CALLBACK_PATH {
        return (
            200,
            "text/html",
            page::bootstrap(BootstrapOptions {
                token_path: TOKEN_PATH.to_owned(),
                provider: Some("DigitalOcean".to_owned()),
            }),
        );
    }
    if request.method == "POST" && request.path == TOKEN_PATH {
        let body = serde_json::from_slice::<CallbackBody>(&request.body).unwrap_or_default();
        let error = body
            .error_description
            .as_deref()
            .filter(|value| !value.is_empty())
            .or_else(|| body.error.as_deref().filter(|value| !value.is_empty()));
        let access = body.access_token.as_deref().filter(|value| !value.is_empty());
        let failure = match error {
            Some(error) => Some(error.to_owned()),
            None => match access {
                None => Some("Missing access token".to_owned()),
                Some(_) if body.state.as_deref() != Some(state) => Some("Invalid OAuth state".to_owned()),
                Some(_) => None,
            },
        };
        if let Some(message) = failure {
            respond(responses, Err(message.clone()));
            return (400, "application/json", serde_json::json!({ "error": message }).to_string());
        }
        let seconds = body
            .expires_in
            .as_deref()
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|seconds| *seconds > 0)
            .unwrap_or(DEFAULT_EXPIRES_SECONDS);
        let expires = opencode_util::time::now_ms() as f64 + seconds as f64 * 1000.0;
        respond(responses, Ok(CallbackToken { access: access.unwrap_or_default().to_owned(), expires }));
        return (200, "application/json", serde_json::json!({ "ok": true }).to_string());
    }
    (404, "", "Not found".to_owned())
}

fn respond(responses: &CallbackResponses, outcome: Result<CallbackToken, String>) {
    if let Some(sender) = responses.lock().expect("lock the DigitalOcean authorization callback").take() {
        let _ = sender.send(outcome);
    }
}

fn bind_error(error: io::Error) -> AuthorizationError {
    if error.kind() == io::ErrorKind::AddrInUse {
        return AuthorizationError::new(io::Error::other(PORT_IN_USE));
    }
    AuthorizationError::new(error)
}

fn generate_state() -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
}

async fn load_routers(http: &dyn HttpSender, user_agent: &str, access: &str) -> Result<Vec<Router>, LoadError> {
    let headers = vec![
        ("authorization".to_owned(), format!("Bearer {access}")),
        ("accept".to_owned(), "application/json".to_owned()),
        ("user-agent".to_owned(), user_agent.to_owned()),
    ];
    let response = http::send(http, http::get(ROUTERS_ENDPOINT, headers), Some(TIMEOUT)).await?;
    let RoutersResponse { mut model_routers } =
        serde_json::from_slice(&response.body).map_err(|error| LoadError::Json(error.to_string()))?;
    if model_routers.iter().any(|router| router.name.is_empty()) {
        return Err(LoadError::Json("the DigitalOcean routers response has an empty name".to_owned()));
    }
    model_routers.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(model_routers)
}
