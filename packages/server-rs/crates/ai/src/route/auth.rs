use serde::Deserialize;
use serde_json::Value;

use crate::Env;
use crate::schema::{AiError, AiErrorReason};

pub const DEFAULT_SIGV4_SERVICE: &str = "bedrock";

#[derive(Debug, Clone)]
pub enum Credential {
    Value { secret: String, source: String },
    Optional { secret: Option<String>, source: String },
    Env(String),
    OrElse(Box<Credential>, Box<Credential>),
}

impl Credential {
    pub fn value(secret: impl Into<String>) -> Self {
        Self::Value { secret: secret.into(), source: "value".to_string() }
    }

    pub fn optional(secret: Option<String>) -> Self {
        Self::Optional { secret, source: "optional value".to_string() }
    }

    pub fn optional_with_source(secret: Option<String>, source: impl Into<String>) -> Self {
        Self::Optional { secret, source: source.into() }
    }

    pub fn env(name: impl Into<String>) -> Self {
        Self::Env(name.into())
    }

    pub fn or_else(self, that: Credential) -> Self {
        Self::OrElse(Box::new(self), Box::new(that))
    }

    pub fn bearer(&self) -> Auth {
        Auth::credential(self.clone(), Render::Bearer)
    }

    pub fn header(&self, name: impl Into<String>) -> Auth {
        Auth::credential(self.clone(), Render::Header(name.into()))
    }

    pub fn bearer_header(&self, name: impl Into<String>) -> Auth {
        Auth::credential(self.clone(), Render::BearerHeader(name.into()))
    }

    pub fn scheme(&self, name: impl Into<String>) -> Auth {
        Auth::credential(self.clone(), Render::Scheme(name.into()))
    }

    pub fn load(&self, env: &dyn Env) -> Result<String, AiError> {
        match self {
            Credential::Value { secret, source } => require_secret(secret, source),
            Credential::Optional { secret, source } => match secret {
                Some(secret) => require_secret(secret, source),
                None => Err(missing_credential(source)),
            },
            Credential::Env(name) => match env.var(name) {
                Some(secret) if !secret.is_empty() => Ok(secret),
                _ => Err(AiError {
                    reason: AiErrorReason::Authentication {
                        message: format!("{name} is not set"),
                        body: None,
                        http: None,
                    },
                }),
            },
            Credential::OrElse(first, second) => first.load(env).or_else(|_| second.load(env)),
        }
    }
}

fn require_secret(secret: &str, source: &str) -> Result<String, AiError> {
    if secret.is_empty() {
        return Err(missing_credential(source));
    }
    Ok(secret.to_string())
}

fn missing_credential(source: &str) -> AiError {
    AiError {
        reason: AiErrorReason::Authentication {
            message: format!("Missing auth credential: {source}"),
            body: None,
            http: None,
        },
    }
}

#[derive(Debug, Clone)]
pub enum Render {
    Bearer,
    Header(String),
    BearerHeader(String),
    Scheme(String),
}

impl Render {
    fn apply(&self, headers: Vec<(String, String)>, secret: &str) -> Vec<(String, String)> {
        match self {
            Render::Bearer => set_header(headers, "authorization", format!("Bearer {secret}")),
            Render::Header(name) => set_header(headers, name, secret.to_string()),
            Render::BearerHeader(name) => set_header(headers, name, format!("Bearer {secret}")),
            Render::Scheme(name) => set_header(headers, "authorization", format!("{name} {secret}")),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Auth {
    None,
    Headers(Vec<(String, String)>),
    Remove(String),
    Credential { credential: Credential, render: Render },
    AndThen(Box<Auth>, Box<Auth>),
    OrElse(Box<Auth>, Box<Auth>),
    SigV4(SigV4Source),
}

impl Auth {
    pub fn credential(credential: Credential, render: Render) -> Self {
        Auth::Credential { credential, render }
    }

    pub fn headers(headers: impl IntoIterator<Item = (String, String)>) -> Self {
        Auth::Headers(headers.into_iter().collect())
    }

    pub fn remove(name: impl Into<String>) -> Self {
        Auth::Remove(name.into())
    }

    pub fn and_then(self, that: Auth) -> Self {
        Auth::AndThen(Box::new(self), Box::new(that))
    }

    pub fn or_else(self, that: Auth) -> Self {
        Auth::OrElse(Box::new(self), Box::new(that))
    }

    pub fn apply(&self, input: AuthInput, env: &dyn Env) -> Result<Vec<(String, String)>, AiError> {
        match self {
            Auth::None => Ok(input.headers),
            Auth::Headers(headers) => Ok(set_all(input.headers, headers)),
            Auth::Remove(name) => Ok(remove_header(input.headers, name)),
            Auth::Credential { credential, render } => {
                let secret = credential.load(env)?;
                Ok(render.apply(input.headers, &secret))
            }
            Auth::AndThen(first, second) => {
                let headers = first.apply(input.clone(), env)?;
                second.apply(AuthInput { headers, ..input }, env)
            }
            Auth::OrElse(first, second) => match first.apply(input.clone(), env) {
                Ok(headers) => Ok(headers),
                Err(_) => second.apply(input, env),
            },
            Auth::SigV4(_) => Err(AiError {
                reason: AiErrorReason::UnsupportedOperation {
                    message: "AWS SigV4 signing is not available yet".to_string(),
                    body: None,
                    http: None,
                    operation: "sigv4".to_string(),
                    provider: None,
                    route: None,
                },
            }),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AuthInput {
    pub method: String,
    pub url: String,
    pub body: String,
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct SigV4Source {
    pub region: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
    pub service: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", rename_all_fields = "camelCase")]
pub enum AuthDescriptor {
    Bearer {
        value: String,
    },
    Header {
        name: String,
        value: String,
    },
    Sigv4 {
        region: String,
        access_key_id: String,
        secret_access_key: String,
        #[serde(default)]
        session_token: Option<String>,
    },
    None,
}

impl From<AuthDescriptor> for Auth {
    fn from(descriptor: AuthDescriptor) -> Self {
        match descriptor {
            AuthDescriptor::Bearer { value } => Credential::value(value).bearer(),
            AuthDescriptor::Header { name, value } => Credential::value(value).header(name),
            AuthDescriptor::Sigv4 { region, access_key_id, secret_access_key, session_token } => {
                Auth::SigV4(SigV4Source {
                    region,
                    access_key_id,
                    secret_access_key,
                    session_token,
                    service: DEFAULT_SIGV4_SERVICE.to_string(),
                })
            }
            AuthDescriptor::None => Auth::None,
        }
    }
}

impl AuthDescriptor {
    pub fn into_auth(self) -> Auth {
        self.into()
    }

    pub fn from_value(value: &Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(value.clone())
    }
}

#[derive(Debug, Clone)]
pub enum ApiKey {
    Value(String),
    Disabled,
}

#[derive(Debug, Clone, Default)]
pub struct ProviderAuthOptions {
    pub auth: Option<Auth>,
    pub api_key: Option<ApiKey>,
}

pub fn api_key(options: ProviderAuthOptions, env_vars: &[&str], apply: impl FnOnce(Credential) -> Auth) -> Auth {
    if let Some(auth) = options.auth {
        return auth;
    }
    if matches!(options.api_key, Some(ApiKey::Disabled)) {
        return Auth::None;
    }
    let key = match options.api_key {
        Some(ApiKey::Value(value)) => Some(value),
        _ => None,
    };
    let credential = env_vars.iter().fold(Credential::optional_with_source(key, "apiKey"), |credential, name| {
        credential.or_else(Credential::env(*name))
    });
    apply(credential)
}

pub fn bearer(options: ProviderAuthOptions, env_vars: &[&str]) -> Auth {
    api_key(options, env_vars, |credential| credential.bearer())
}

pub fn set_header(
    mut headers: Vec<(String, String)>,
    name: impl AsRef<str>,
    value: impl Into<String>,
) -> Vec<(String, String)> {
    let name = name.as_ref().to_lowercase();
    let value = value.into();
    headers.retain(|(existing, _)| existing.to_lowercase() != name);
    headers.push((name, value));
    headers
}

fn set_all(headers: Vec<(String, String)>, extra: &[(String, String)]) -> Vec<(String, String)> {
    extra.iter().fold(headers, |headers, (name, value)| set_header(headers, name, value.clone()))
}

fn remove_header(headers: Vec<(String, String)>, name: &str) -> Vec<(String, String)> {
    let name = name.to_lowercase();
    headers.into_iter().filter(|(existing, _)| existing.to_lowercase() != name).collect()
}
