use std::sync::Arc;

use crate::Env;
use crate::protocols::shared::invalid_request;
use crate::providers::Settings;
use crate::route::auth::{Auth, Credential, CredentialSource, Render};
use crate::route::endpoint::Endpoint;
use crate::route::model::Route;
use crate::schema::{AiError, AiErrorReason, RequestOptions, merge_request_options};

pub const ID: &str = "google-vertex";
pub const METADATA_KEY: &str = "vertex";
pub const CONNECTION_KEYS: [&str; 5] = ["accessToken", "apiKey", "baseURL", "location", "project"];

const ADC_SOURCE: &str = "Google Application Default Credentials";

pub fn project(value: Option<&str>, env: &dyn Env) -> Option<String> {
    value
        .map(str::to_string)
        .or_else(|| env.var("GOOGLE_VERTEX_PROJECT"))
        .or_else(|| env.var("GOOGLE_CLOUD_PROJECT"))
        .or_else(|| env.var("GCP_PROJECT"))
        .or_else(|| env.var("GCLOUD_PROJECT"))
}

pub fn location(value: Option<&str>, env: &dyn Env) -> String {
    value
        .map(str::to_string)
        .or_else(|| env.var("GOOGLE_VERTEX_LOCATION"))
        .or_else(|| env.var("GOOGLE_CLOUD_LOCATION"))
        .or_else(|| env.var("VERTEX_LOCATION"))
        .unwrap_or_else(|| "global".to_string())
}

pub fn host(location: &str) -> String {
    match location {
        "global" => "aiplatform.googleapis.com".to_string(),
        "eu" | "us" => format!("aiplatform.{location}.rep.googleapis.com"),
        _ => format!("{location}-aiplatform.googleapis.com"),
    }
}

pub fn require_project(value: Option<&str>) -> Result<String, AiError> {
    match value.filter(|value| !value.is_empty()) {
        Some(value) => Ok(value.to_string()),
        None => Err(invalid_request("Google Vertex requires a project when baseURL is not configured")),
    }
}

#[derive(Default, Clone)]
pub struct ApiKeyInput {
    pub api_key: Option<String>,
    pub access_token: Option<String>,
    pub auth: Option<Auth>,
    pub project: Option<String>,
    pub location: Option<String>,
}

pub fn api_key(input: &ApiKeyInput, env: &dyn Env) -> Result<Option<String>, AiError> {
    if input.api_key.is_some() && (input.access_token.is_some() || input.auth.is_some()) {
        return Err(invalid_request("Google Vertex apiKey cannot be combined with accessToken or auth"));
    }
    if input.access_token.is_some() || input.auth.is_some() {
        return Ok(None);
    }
    if let Some(api_key) = &input.api_key {
        return Ok(Some(api_key.clone()));
    }
    if input.project.is_some() || input.location.is_some() {
        return Ok(None);
    }
    Ok(env.var("GOOGLE_VERTEX_API_KEY"))
}

#[derive(Default, Clone)]
pub struct OAuthInput {
    pub access_token: Option<String>,
    pub auth: Option<Auth>,
}

#[derive(Debug)]
pub struct MissingAdc;

impl CredentialSource for MissingAdc {
    fn load(&self, _env: &dyn Env) -> Result<String, AiError> {
        Err(AiError {
            reason: AiErrorReason::Authentication { message: ADC_SOURCE.to_string(), body: None, http: None },
        })
    }
}

pub fn adc() -> Auth {
    Auth::credential(Credential::Source(Arc::new(MissingAdc)), Render::Bearer)
}

pub fn oauth(input: &OAuthInput, _project: Option<&str>) -> Result<Auth, AiError> {
    if input.access_token.is_some() && input.auth.is_some() {
        return Err(invalid_request("Google Vertex accessToken cannot be combined with auth"));
    }
    if let Some(auth) = &input.auth {
        return Ok(auth.clone());
    }
    if let Some(access_token) = &input.access_token {
        return Ok(Credential::value(access_token.clone()).bearer());
    }
    Ok(adc())
}

#[derive(Default)]
pub struct OAuthRouteInput {
    pub api_key: Option<String>,
    pub access_token: Option<String>,
    pub auth: Option<Auth>,
    pub base_url: Option<String>,
    pub location: Option<String>,
    pub project: Option<String>,
    pub options: Option<RequestOptions>,
}

pub fn oauth_settings(label: &str, settings: &Settings) -> Result<OAuthRouteInput, AiError> {
    if settings.value("apiKey").is_some() {
        return Err(invalid_request(format!("Google Vertex {label} does not support API keys")));
    }
    Ok(OAuthRouteInput {
        api_key: settings.string("apiKey").map(str::to_string),
        access_token: settings.string("accessToken").map(str::to_string),
        auth: None,
        base_url: settings.string("baseURL").map(str::to_string),
        location: settings.string("location").map(str::to_string),
        project: settings.string("project").map(str::to_string),
        options: settings.request_options(&CONNECTION_KEYS),
    })
}

pub fn request_type_header(request: &crate::schema::LlmRequest) -> Vec<(String, String)> {
    match request
        .provider_options
        .as_ref()
        .and_then(|options| options.get("serviceTier"))
        .and_then(serde_json::Value::as_str)
    {
        Some(service_tier) => vec![("x-vertex-ai-llm-shared-request-type".to_string(), service_tier.to_string())],
        None => Vec::new(),
    }
}

pub fn oauth_route(
    route: Route,
    label: &str,
    input: OAuthRouteInput,
    url: impl Fn(&str, &str) -> String,
    env: &dyn Env,
) -> Result<Route, AiError> {
    if input.api_key.is_some() {
        return Err(invalid_request(format!("Google Vertex {label} does not support API keys")));
    }
    let resolved_location = location(input.location.as_deref(), env);
    let resolved_project = project(input.project.as_deref(), env);
    let base_url = match input.base_url.as_deref() {
        Some(base_url) => base_url.to_string(),
        None => url(&require_project(resolved_project.as_deref())?, &resolved_location),
    };
    let auth = oauth(
        &OAuthInput { access_token: input.access_token.clone(), auth: input.auth.clone() },
        resolved_project.as_deref(),
    )?;
    let options = merge_request_options(&[route.options.as_ref(), input.options.as_ref()]);
    Ok(Route { endpoint: Endpoint { base_url: Some(base_url), ..route.endpoint.clone() }, auth, options, ..route })
}

pub fn settings_api_key_input(settings: &Settings) -> ApiKeyInput {
    ApiKeyInput {
        api_key: settings.string("apiKey").map(str::to_string),
        access_token: settings.string("accessToken").map(str::to_string),
        auth: None,
        project: settings.string("project").map(str::to_string),
        location: settings.string("location").map(str::to_string),
    }
}
