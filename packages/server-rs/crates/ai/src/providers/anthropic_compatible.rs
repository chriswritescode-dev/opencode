use std::sync::Arc;

use crate::Env;
use crate::protocols::anthropic_messages::AnthropicMessages;
use crate::protocols::shared::invalid_request;
use crate::providers::{RuntimeModel, Settings, language_model};
use crate::route::auth::{Credential, ProviderAuthOptions, api_key};
use crate::route::endpoint::{Endpoint, PathPart};
use crate::schema::{AiError, StringMap};

pub const ID: &str = "anthropic-compatible";
const ANTHROPIC_ID: &str = "anthropic";

pub fn model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    if settings.value("apiKey").is_some() && settings.string("authToken").is_some() {
        return Err(invalid_request("Anthropic-compatible apiKey cannot be combined with authToken"));
    }
    let provider = settings.string("provider").unwrap_or(ID);
    let base_url = settings
        .string("baseURL")
        .ok_or_else(|| invalid_request("Anthropic-compatible providers require a baseURL"))?;
    let (route_id, metadata_key) = match provider {
        ANTHROPIC_ID => ("anthropic-messages", ANTHROPIC_ID),
        _ => ("anthropic-compatible-messages", provider),
    };
    let auth = match settings.string("authToken") {
        Some(token) => Credential::value(token).bearer(),
        None => api_key(ProviderAuthOptions { auth: None, api_key: settings.api_key() }, &[], |credential| {
            credential.header("x-api-key")
        }),
    };
    Ok(language_model(
        model_id,
        provider,
        route_id,
        Some(metadata_key),
        Arc::new(AnthropicMessages),
        Endpoint { base_url: Some(base_url.to_string()), path: PathPart::Default, query: StringMap::new() },
        auth,
        settings.request_options(&["apiKey", "authToken", "baseURL", "provider"]),
        None,
        None,
    ))
}
