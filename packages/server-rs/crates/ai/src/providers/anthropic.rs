use std::sync::Arc;

use crate::Env;
use crate::protocols::anthropic_messages::{AnthropicMessages, DEFAULT_BASE_URL};
use crate::protocols::shared::invalid_request;
use crate::providers::{RuntimeModel, Settings, language_model};
use crate::route::auth::{Credential, ProviderAuthOptions, api_key};
use crate::route::endpoint::{Endpoint, PathPart};
use crate::schema::{AiError, StringMap};

pub const ID: &str = "anthropic";

pub fn model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    if settings.value("apiKey").is_some() && settings.string("authToken").is_some() {
        return Err(invalid_request("Anthropic apiKey cannot be combined with authToken"));
    }
    let auth = match settings.string("authToken") {
        Some(token) => Credential::value(token).bearer(),
        None => api_key(
            ProviderAuthOptions { auth: None, api_key: settings.api_key() },
            &["ANTHROPIC_API_KEY"],
            |credential| credential.header("x-api-key"),
        ),
    };
    Ok(language_model(
        model_id,
        ID,
        "anthropic-messages",
        Some("anthropic"),
        Arc::new(AnthropicMessages::default()),
        Endpoint {
            base_url: Some(settings.string("baseURL").unwrap_or(DEFAULT_BASE_URL).to_string()),
            path: PathPart::Default,
            query: StringMap::new(),
        },
        auth,
        settings.request_options(&["apiKey", "authToken", "baseURL"]),
        None,
        None,
    ))
}
