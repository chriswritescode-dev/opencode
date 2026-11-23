use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use crate::Env;
use crate::clock::utc_datetime;
use crate::schema::{AiError, AiErrorReason};

pub const DEFAULT_SIGV4_SERVICE: &str = "bedrock";

const UNSIGNABLE_HEADERS: [&str; 9] = [
    "authorization",
    "content-type",
    "content-length",
    "user-agent",
    "presigned-expires",
    "expect",
    "x-amzn-trace-id",
    "range",
    "connection",
];

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
            Auth::SigV4(source) => {
                let headers = set_header(input.headers, "content-type", "application/json");
                sign_sigv4(source, &input.method, &input.url, &input.body, headers, input.now_ms, "Bedrock Converse")
            }
        }
    }
}

fn sign_sigv4(
    source: &SigV4Source,
    method: &str,
    url: &str,
    body: &str,
    headers: Vec<(String, String)>,
    now_ms: u64,
    name: &str,
) -> Result<Vec<(String, String)>, AiError> {
    sign_sigv4_request(source, method, url, body, headers, now_ms).map_err(|detail| sigv4_failure(name, detail))
}

fn sign_sigv4_request(
    source: &SigV4Source,
    method: &str,
    url: &str,
    body: &str,
    headers: Vec<(String, String)>,
    now_ms: u64,
) -> Result<Vec<(String, String)>, String> {
    let url = url::Url::parse(url).map_err(|error| error.to_string())?;
    let host = sigv4_host(&url).ok_or_else(|| "url has no host".to_string())?;
    let datetime = utc_datetime(now_ms as i64).amz();
    let date = &datetime[..8];
    let headers = set_header(headers, "x-amz-date", datetime.clone());
    let headers = match &source.session_token {
        Some(token) => set_header(headers, "x-amz-security-token", token.clone()),
        None => headers,
    };
    let headers: Vec<(String, String)> =
        headers.into_iter().filter(|(name, _)| !name.eq_ignore_ascii_case("host")).collect();
    let signable = headers.iter().fold(BTreeMap::from([("host".to_string(), host)]), |mut signable, (name, value)| {
        let name = name.to_lowercase();
        if !UNSIGNABLE_HEADERS.contains(&name.as_str()) {
            signable.insert(name, collapse_whitespace(value));
        }
        signable
    });
    let signed_headers = signable.keys().map(String::as_str).collect::<Vec<_>>().join(";");
    let canonical_headers =
        signable.iter().map(|(name, value)| format!("{name}:{value}")).collect::<Vec<_>>().join("\n");
    let canonical_path = encode_rfc3986(&encode_uri_component(&collapse_slashes(url.path())).replace("%2F", "/"));
    let canonical_query = canonical_query(&url);
    let canonical_request = format!(
        "{}\n{canonical_path}\n{canonical_query}\n{canonical_headers}\n\n{signed_headers}\n{}",
        method.to_uppercase(),
        sha256_hex(body.as_bytes())
    );
    let credential_string = format!("{date}/{}/{}/aws4_request", source.region, source.service);
    let string_to_sign =
        format!("AWS4-HMAC-SHA256\n{datetime}\n{credential_string}\n{}", sha256_hex(canonical_request.as_bytes()));
    let signature = hex(&hmac_sha256(&sigv4_key(source, date), string_to_sign.as_bytes()));
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{credential_string}, SignedHeaders={signed_headers}, Signature={signature}",
        source.access_key_id
    );
    Ok(set_header(headers, "authorization", authorization))
}

fn sigv4_host(url: &url::Url) -> Option<String> {
    let host = url.host_str()?;
    match url.port() {
        Some(port) => Some(format!("{host}:{port}")),
        None => Some(host.to_string()),
    }
}

fn sigv4_key(source: &SigV4Source, date: &str) -> Vec<u8> {
    let mut key = hmac_sha256(format!("AWS4{}", source.secret_access_key).as_bytes(), date.as_bytes());
    for part in [source.region.as_str(), source.service.as_str(), "aws4_request"] {
        key = hmac_sha256(&key, part.as_bytes());
    }
    key
}

fn canonical_query(url: &url::Url) -> String {
    let mut pairs: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| !key.is_empty())
        .map(|(key, value)| {
            (encode_rfc3986(&encode_uri_component(&key)), encode_rfc3986(&encode_uri_component(&value)))
        })
        .collect();
    pairs.sort();
    pairs.iter().map(|(key, value)| format!("{key}={value}")).collect::<Vec<_>>().join("&")
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn collapse_slashes(value: &str) -> String {
    value.chars().fold(String::with_capacity(value.len()), |mut collapsed, character| {
        if character != '/' || !collapsed.ends_with('/') {
            collapsed.push(character);
        }
        collapsed
    })
}

pub(crate) fn encode_uri_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')' => {
                encoded.push(byte as char);
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

fn encode_rfc3986(value: &str) -> String {
    value.replace('!', "%21").replace('\'', "%27").replace('(', "%28").replace(')', "%29").replace('*', "%2A")
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
    ring::hmac::sign(&key, data).as_ref().to_vec()
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn sigv4_failure(name: &str, detail: String) -> AiError {
    AiError {
        reason: AiErrorReason::InvalidRequest {
            message: format!("{name} SigV4 signing failed: {detail}"),
            body: None,
            http: None,
            parameter: None,
            classification: None,
        },
    }
}

#[derive(Debug, Clone)]
pub struct AuthInput {
    pub method: String,
    pub url: String,
    pub body: String,
    pub headers: Vec<(String, String)>,
    pub now_ms: u64,
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
