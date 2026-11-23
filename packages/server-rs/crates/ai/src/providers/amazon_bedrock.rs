use std::sync::Arc;

use crate::Env;
use crate::protocols::bedrock_converse::BedrockConverse;
use crate::protocols::shared::invalid_request;
use crate::protocols::utils::bedrock_auth;
use crate::providers::{RuntimeModel, Settings, language_model};
use crate::route::endpoint::{Endpoint, PathPart};
use crate::schema::{AiError, GenerationOptions, RequestOptions, StringMap, merge_request_options};

pub const ID: &str = "amazon-bedrock";

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct CredentialsInput {
    region: String,
    access_key_id: String,
    secret_access_key: String,
    #[serde(default)]
    session_token: Option<String>,
}

pub fn model(model_id: &str, settings: &Settings, env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let api_key = settings.string("apiKey").map(str::to_string);
    let mode = match settings.string("auth") {
        Some("bearer") => Some(bedrock_auth::AuthMode::Bearer),
        Some("sigv4") => Some(bedrock_auth::AuthMode::Sigv4),
        _ => None,
    };
    if matches!(mode, Some(bedrock_auth::AuthMode::Bearer))
        && api_key.is_none()
        && env.var("AWS_BEARER_TOKEN_BEDROCK").is_none()
    {
        return Err(invalid_request("Amazon Bedrock bearer auth requires apiKey"));
    }
    if matches!(mode, Some(bedrock_auth::AuthMode::Sigv4)) && api_key.is_some() {
        return Err(invalid_request("Amazon Bedrock SigV4 auth does not accept apiKey"));
    }
    let credentials = credentials(settings);
    let region = bedrock_auth::resolve_region(settings.string("region"), credentials.as_ref(), env);
    let base_url = settings
        .string("baseURL")
        .map(str::to_string)
        .unwrap_or_else(|| format!("https://bedrock-runtime.{region}.amazonaws.com"));
    let auth = bedrock_auth::resolve_auth(
        bedrock_auth::ResolveAuthInput { api_key, credentials, profile: settings.optional_string("profile") },
        &region,
        bedrock_auth::ResolveAuthOptions { service: None, mode },
        env,
    )?;
    let options = settings.request_options(&["apiKey", "auth", "baseURL", "credentials", "profile", "region", "topP"]);
    let top_p = settings.optional_number("topP").map(|top_p| RequestOptions {
        generation: GenerationOptions { top_p: Some(top_p), ..GenerationOptions::default() },
        ..RequestOptions::default()
    });
    Ok(language_model(
        model_id,
        ID,
        "bedrock-converse",
        Some("bedrock"),
        Arc::new(BedrockConverse),
        Endpoint { base_url: Some(base_url), path: PathPart::Default, query: StringMap::new() },
        auth,
        merge_request_options(&[options.as_ref(), top_p.as_ref()]),
        None,
        None,
    ))
}

fn credentials(settings: &Settings) -> Option<bedrock_auth::Credentials> {
    let input: CredentialsInput = serde_json::from_value(settings.value("credentials")?.clone()).ok()?;
    Some(bedrock_auth::Credentials {
        region: input.region,
        access_key_id: input.access_key_id,
        secret_access_key: input.secret_access_key,
        session_token: input.session_token,
    })
}
