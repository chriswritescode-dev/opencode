use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use bytes::Bytes;
use futures_util::StreamExt;
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use serde_json::Value;
use url::form_urlencoded::Serializer;

use crate::Env;
use crate::ExecutionContext;
use crate::route::auth::CredentialSource;
use crate::route::executor::{BoxFuture, ByteStream, HttpResponse, HttpSender, PreparedHttp};
use crate::schema::{AiError, AiErrorReason};

pub const SOURCE: &str = "Google Application Default Credentials";

const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
const FORM_CONTENT_TYPE: &str = "application/x-www-form-urlencoded;charset=UTF-8";
const JWT_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";
const EAGER_REFRESH_MS: u64 = 300_000;
const METADATA_TIMEOUT: Duration = Duration::from_millis(3000);
const MAX_TOKEN_BODY_BYTES: usize = 1024 * 1024;
const PKCS8_BEGIN: &str = "-----BEGIN PRIVATE KEY-----";
const PKCS8_END: &str = "-----END PRIVATE KEY-----";
const PKCS1_BEGIN: &str = "-----BEGIN RSA PRIVATE KEY-----";
const PKCS1_END: &str = "-----END RSA PRIVATE KEY-----";

pub trait CredentialFiles: Send + Sync + std::fmt::Debug {
    fn exists(&self, path: &str) -> bool;
    fn read(&self, path: &str) -> Option<String>;
}

#[derive(Debug, Default)]
pub struct ProcessFiles;

impl CredentialFiles for ProcessFiles {
    fn exists(&self, path: &str) -> bool {
        Path::new(path).is_file()
    }

    fn read(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }
}

#[derive(Clone)]
enum AdcCredential {
    AuthorizedUser { client_id: String, client_secret: String, refresh_token: String },
    ServiceAccount { client_email: String, private_key: String },
    Metadata { base_url: String },
}

#[derive(Clone)]
struct CachedToken {
    access: String,
    expiry_ms: Option<u64>,
}

#[derive(Clone)]
struct Session {
    credential: AdcCredential,
    token: Option<CachedToken>,
}

pub struct GoogleAdc {
    project: Option<String>,
    files: Arc<dyn CredentialFiles>,
    state: Arc<Mutex<Option<Session>>>,
    refresh: Arc<tokio::sync::Mutex<()>>,
}

impl GoogleAdc {
    pub fn new(project: Option<String>) -> Self {
        Self::with_files(project, Arc::new(ProcessFiles))
    }

    pub fn with_files(project: Option<String>, files: Arc<dyn CredentialFiles>) -> Self {
        Self { project, files, state: Arc::new(Mutex::new(None)), refresh: Arc::new(tokio::sync::Mutex::new(())) }
    }
}

impl std::fmt::Debug for GoogleAdc {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GoogleAdc")
            .field("project", &self.project)
            .field("files", &self.files)
            .finish_non_exhaustive()
    }
}

impl CredentialSource for GoogleAdc {
    fn load(&self, _env: &dyn Env) -> Result<String, AiError> {
        let state = self.state.lock().expect("Google ADC state lock poisoned");
        state
            .as_ref()
            .and_then(|session| session.token.as_ref())
            .map(|token| token.access.clone())
            .ok_or_else(adc_error)
    }

    fn prepare(&self, context: &ExecutionContext, sender: &Arc<dyn HttpSender>) -> BoxFuture<Result<(), AiError>> {
        let cached = self.state.lock().expect("Google ADC state lock poisoned").clone();
        if cached
            .as_ref()
            .is_some_and(|session| session.token.as_ref().is_some_and(|token| fresh(token, context.clock.now_ms())))
        {
            return Box::pin(async { Ok(()) });
        }
        let files = self.files.clone();
        let state = self.state.clone();
        let refresh = self.refresh.clone();
        let context = context.clone();
        let sender = sender.clone();
        Box::pin(async move {
            let _guard = refresh.lock().await;
            let cached = state.lock().expect("Google ADC state lock poisoned").clone();
            if cached
                .as_ref()
                .is_some_and(|session| session.token.as_ref().is_some_and(|token| fresh(token, context.clock.now_ms())))
            {
                return Ok(());
            }
            let credential = match cached {
                Some(session) => session.credential,
                None => resolve(files.as_ref(), context.env.as_ref(), &sender).await?,
            };
            let token = request_token(&credential, context.clock.now_ms(), &sender).await?;
            *state.lock().expect("Google ADC state lock poisoned") = Some(Session { credential, token: Some(token) });
            Ok(())
        })
    }
}

pub fn expand_cloud_sdk_config(config: &str, home: Option<&str>) -> String {
    let Some(home) = home else { return config.to_string() };
    if config == "~" {
        return home.to_string();
    }
    match config.strip_prefix("~/") {
        Some(rest) => Path::new(home).join(rest).to_string_lossy().into_owned(),
        None => config.to_string(),
    }
}

async fn resolve(
    files: &dyn CredentialFiles,
    env: &dyn Env,
    sender: &Arc<dyn HttpSender>,
) -> Result<AdcCredential, AiError> {
    if let Some(path) = credential_path(env) {
        return read_credential(files, &path);
    }
    if let Some(path) = well_known_path(files, env) {
        return read_credential(files, &path);
    }
    metadata_credential(env, sender).await
}

fn credential_path(env: &dyn Env) -> Option<String> {
    ["GOOGLE_APPLICATION_CREDENTIALS", "google_application_credentials"]
        .into_iter()
        .filter_map(|name| env.var(name))
        .find(|path| !path.is_empty())
}

fn well_known_path(files: &dyn CredentialFiles, env: &dyn Env) -> Option<String> {
    if let Some(config) = env.var("CLOUDSDK_CONFIG").filter(|value| !value.is_empty()) {
        let home = env.var("HOME").or_else(|| env.var("USERPROFILE"));
        let directory = expand_cloud_sdk_config(&config, home.as_deref());
        let file = Path::new(&directory).join("application_default_credentials.json");
        let file = file.to_string_lossy().into_owned();
        return files.exists(&file).then_some(file);
    }
    let location = if cfg!(windows) {
        env.var("APPDATA").map(std::path::PathBuf::from)
    } else {
        env.var("HOME").map(|home| Path::new(&home).join(".config"))
    }?;
    let file = location.join("gcloud").join("application_default_credentials.json");
    let file = file.to_string_lossy().into_owned();
    files.exists(&file).then_some(file)
}

fn read_credential(files: &dyn CredentialFiles, path: &str) -> Result<AdcCredential, AiError> {
    let text = files.read(path).ok_or_else(adc_error)?;
    parse_credential(&text)
}

fn parse_credential(text: &str) -> Result<AdcCredential, AiError> {
    let value: Value = serde_json::from_str(text).map_err(|_| adc_error())?;
    match value.get("type").and_then(Value::as_str) {
        Some("authorized_user") => Ok(AdcCredential::AuthorizedUser {
            client_id: require_field(&value, "client_id")?,
            client_secret: require_field(&value, "client_secret")?,
            refresh_token: require_field(&value, "refresh_token")?,
        }),
        Some("service_account") => Ok(AdcCredential::ServiceAccount {
            client_email: require_field(&value, "client_email")?,
            private_key: require_field(&value, "private_key")?,
        }),
        _ => Err(adc_error()),
    }
}

fn require_field(value: &Value, name: &str) -> Result<String, AiError> {
    match value.get(name).and_then(Value::as_str).filter(|field| !field.is_empty()) {
        Some(field) => Ok(field.to_string()),
        None => Err(adc_error()),
    }
}

async fn metadata_credential(env: &dyn Env, sender: &Arc<dyn HttpSender>) -> Result<AdcCredential, AiError> {
    let base_url = metadata_base_url(env);
    let detection =
        env.var("METADATA_SERVER_DETECTION").map(|value| value.trim().to_lowercase()).filter(|value| !value.is_empty());
    match detection.as_deref() {
        Some("none") => Err(adc_error()),
        Some("assume-present") => Ok(AdcCredential::Metadata { base_url }),
        Some("bios-only" | "ping-only") | None => {
            if metadata_available(sender, &base_url).await {
                return Ok(AdcCredential::Metadata { base_url });
            }
            Err(adc_error())
        }
        Some(_) => Err(adc_error()),
    }
}

fn metadata_base_url(env: &dyn Env) -> String {
    let base = env
        .var("GCE_METADATA_IP")
        .filter(|value| !value.is_empty())
        .or_else(|| env.var("GCE_METADATA_HOST").filter(|value| !value.is_empty()))
        .unwrap_or_else(|| "http://169.254.169.254".to_string());
    let base =
        if base.starts_with("http://") || base.starts_with("https://") { base } else { format!("http://{base}") };
    base.trim_end_matches('/').to_string()
}

async fn metadata_available(sender: &Arc<dyn HttpSender>, base_url: &str) -> bool {
    let request = PreparedHttp::new(
        "GET",
        format!("{base_url}/computeMetadata/v1/instance"),
        vec![("metadata-flavor".to_string(), "Google".to_string())],
        Bytes::new(),
    );
    match tokio::time::timeout(METADATA_TIMEOUT, sender.send(request)).await {
        Ok(Ok(response)) => (200..300).contains(&response.status) && has_metadata_flavor(&response.headers),
        Ok(Err(_)) | Err(_) => false,
    }
}

fn has_metadata_flavor(headers: &[(String, String)]) -> bool {
    headers.iter().any(|(name, value)| name.eq_ignore_ascii_case("metadata-flavor") && value == "Google")
}

async fn request_token(
    credential: &AdcCredential,
    now_ms: u64,
    sender: &Arc<dyn HttpSender>,
) -> Result<CachedToken, AiError> {
    let (request, expiry_base_ms, metadata) = match credential {
        AdcCredential::AuthorizedUser { client_id, client_secret, refresh_token } => {
            (authorized_user_request(client_id, client_secret, refresh_token), now_ms, false)
        }
        AdcCredential::ServiceAccount { client_email, private_key } => {
            let iat = now_ms / 1000;
            (service_account_request(client_email, private_key, iat)?, iat * 1000, false)
        }
        AdcCredential::Metadata { base_url } => (metadata_request(base_url), now_ms, true),
    };
    let response = match metadata {
        true => tokio::time::timeout(METADATA_TIMEOUT, sender.send(request))
            .await
            .map_err(|_| adc_error())?
            .map_err(|_| adc_error())?,
        false => sender.send(request).await.map_err(|_| adc_error())?,
    };
    token_from_response(response, expiry_base_ms, metadata).await
}

fn authorized_user_request(client_id: &str, client_secret: &str, refresh_token: &str) -> PreparedHttp {
    let body = form_body(&[
        ("refresh_token", refresh_token),
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("grant_type", "refresh_token"),
    ]);
    token_request(body)
}

fn service_account_request(client_email: &str, private_key: &str, iat: u64) -> Result<PreparedHttp, AiError> {
    let header = crate::json::encode_body(&serde_json::json!({ "alg": "RS256" }));
    let payload = crate::json::encode_body(&serde_json::json!({
        "iss": client_email,
        "scope": SCOPE,
        "aud": TOKEN_URL,
        "exp": iat + 3600,
        "iat": iat,
    }));
    let signing_input = format!("{}.{}", URL_SAFE_NO_PAD.encode(header), URL_SAFE_NO_PAD.encode(payload));
    let key = parse_key(private_key)?;
    let mut signature = vec![0; key.public().modulus_len()];
    key.sign(&RSA_PKCS1_SHA256, &SystemRandom::new(), signing_input.as_bytes(), &mut signature)
        .map_err(|_| adc_error())?;
    let assertion = format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(signature));
    Ok(token_request(form_body(&[("grant_type", JWT_GRANT_TYPE), ("assertion", &assertion)])))
}

fn parse_key(private_key: &str) -> Result<RsaKeyPair, AiError> {
    if private_key.contains(PKCS8_BEGIN) {
        let der = pem_body(private_key, PKCS8_BEGIN, PKCS8_END)?;
        return RsaKeyPair::from_pkcs8(&der).or_else(|_| RsaKeyPair::from_der(&der)).map_err(|_| adc_error());
    }
    let der = pem_body(private_key, PKCS1_BEGIN, PKCS1_END)?;
    RsaKeyPair::from_der(&der).map_err(|_| adc_error())
}

fn pem_body(pem: &str, begin: &str, end: &str) -> Result<Vec<u8>, AiError> {
    let body =
        pem.split_once(begin).and_then(|(_, rest)| rest.split_once(end)).map(|(body, _)| body).ok_or_else(adc_error)?;
    let encoded: String = body.chars().filter(|character| !character.is_whitespace()).collect();
    STANDARD.decode(encoded).map_err(|_| adc_error())
}

fn metadata_request(base_url: &str) -> PreparedHttp {
    let query = form_body(&[("scopes", SCOPE)]);
    PreparedHttp::new(
        "GET",
        format!("{base_url}/computeMetadata/v1/instance/service-accounts/default/token?{query}"),
        vec![("metadata-flavor".to_string(), "Google".to_string())],
        Bytes::new(),
    )
}

fn token_request(body: String) -> PreparedHttp {
    PreparedHttp::new(
        "POST",
        TOKEN_URL,
        vec![("content-type".to_string(), FORM_CONTENT_TYPE.to_string())],
        Bytes::from(body),
    )
}

fn form_body(pairs: &[(&str, &str)]) -> String {
    let mut serializer = Serializer::new(String::new());
    for (name, value) in pairs {
        serializer.append_pair(name, value);
    }
    serializer.finish()
}

async fn token_from_response(
    response: HttpResponse,
    expiry_base_ms: u64,
    metadata: bool,
) -> Result<CachedToken, AiError> {
    if !(200..300).contains(&response.status) {
        return Err(adc_error());
    }
    if metadata && !has_metadata_flavor(&response.headers) {
        return Err(adc_error());
    }
    let text = read_body(response.body).await?;
    let value: Value = serde_json::from_str(&text).map_err(|_| adc_error())?;
    let access = value
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|access| !access.is_empty())
        .ok_or_else(adc_error)?
        .to_string();
    let expiry_ms = value
        .get("expires_in")
        .and_then(Value::as_f64)
        .filter(|seconds| seconds.is_finite())
        .map(|seconds| expiry_base_ms.saturating_add((seconds * 1000.0) as u64));
    Ok(CachedToken { access, expiry_ms })
}

async fn read_body(mut body: ByteStream) -> Result<String, AiError> {
    let mut content = Vec::new();
    while content.len() < MAX_TOKEN_BODY_BYTES {
        let Some(chunk) = body.next().await else { break };
        let chunk = chunk.map_err(|_| adc_error())?;
        let remaining = MAX_TOKEN_BODY_BYTES - content.len();
        content.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
    }
    Ok(String::from_utf8_lossy(&content).into_owned())
}

fn fresh(token: &CachedToken, now_ms: u64) -> bool {
    match token.expiry_ms {
        None => true,
        Some(expiry_ms) => expiry_ms > now_ms + EAGER_REFRESH_MS,
    }
}

fn adc_error() -> AiError {
    AiError {
        reason: AiErrorReason::Authentication {
            message: format!("Missing auth credential: {SOURCE}"),
            body: None,
            http: None,
        },
    }
}
