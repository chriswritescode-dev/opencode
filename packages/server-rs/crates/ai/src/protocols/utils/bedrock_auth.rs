use crate::Env;
use crate::route::auth::{Auth, Credential, DEFAULT_SIGV4_SERVICE, SigV4Source};
use crate::schema::{AiError, AiErrorReason};

pub use crate::route::auth::DEFAULT_SIGV4_NAME;

#[derive(Debug, Clone, PartialEq)]
pub struct Credentials {
    pub region: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}

pub fn resolve_region(region: Option<&str>, credentials: Option<&Credentials>, env: &dyn Env) -> String {
    if let Some(region) = region {
        return region.to_string();
    }
    if let Some(credentials) = credentials {
        return credentials.region.clone();
    }
    if let Some(region) = env.var("AWS_REGION") {
        return region;
    }
    if let Some(region) = env.var("AWS_DEFAULT_REGION") {
        return region;
    }
    "us-east-1".to_string()
}

pub fn credentials(value: Option<&serde_json::Value>) -> Option<Credentials> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct CredentialsInput {
        region: String,
        access_key_id: String,
        secret_access_key: String,
        #[serde(default)]
        session_token: Option<String>,
    }

    let input: CredentialsInput = serde_json::from_value(value?.clone()).ok()?;
    Some(Credentials {
        region: input.region,
        access_key_id: input.access_key_id,
        secret_access_key: input.secret_access_key,
        session_token: input.session_token,
    })
}

pub fn sigv4(credentials: &Credentials, service: Option<&str>, name: Option<&str>) -> Auth {
    Auth::SigV4(SigV4Source {
        region: credentials.region.clone(),
        access_key_id: credentials.access_key_id.clone(),
        secret_access_key: credentials.secret_access_key.clone(),
        session_token: credentials.session_token.clone(),
        service: service.unwrap_or(DEFAULT_SIGV4_SERVICE).to_string(),
        name: name.unwrap_or(DEFAULT_SIGV4_NAME).to_string(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    Bearer,
    Sigv4,
}

#[derive(Debug, Clone, Default)]
pub struct ResolveAuthOptions {
    pub service: Option<String>,
    pub name: Option<String>,
    pub mode: Option<AuthMode>,
}

#[derive(Debug, Clone, Default)]
pub struct ResolveAuthInput {
    pub api_key: Option<String>,
    pub credentials: Option<Credentials>,
    pub profile: Option<String>,
}

pub fn resolve_auth(
    input: ResolveAuthInput,
    region: &str,
    options: ResolveAuthOptions,
    env: &dyn Env,
) -> Result<Auth, AiError> {
    let api_key = match options.mode {
        Some(AuthMode::Sigv4) => None,
        _ => input.api_key.or_else(|| env.var("AWS_BEARER_TOKEN_BEDROCK")),
    };
    if let Some(api_key) = api_key {
        return Ok(Credential::value(api_key).bearer());
    }
    if let Some(credentials) = input.credentials {
        let credentials = Credentials { region: region.to_string(), ..credentials };
        return Ok(sigv4(&credentials, options.service.as_deref(), options.name.as_deref()));
    }
    Err(AiError {
        reason: AiErrorReason::Authentication {
            message: "AWS default credential chain failed: the default credential chain is not available".to_string(),
            body: None,
            http: None,
        },
    })
}
