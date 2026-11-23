use std::sync::Arc;

use crate::Env;
use crate::protocols::gemini::{DEFAULT_BASE_URL, Gemini};
use crate::protocols::google_interactions::{self, GoogleInteractions};
use crate::providers::{RuntimeModel, Settings, language_model};
use crate::route::auth::{ProviderAuthOptions, api_key};
use crate::route::endpoint::{Endpoint, PathPart};
use crate::schema::{AiError, StringMap};

pub const ID: &str = "google";

pub fn model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let auth = api_key(
        ProviderAuthOptions { auth: None, api_key: settings.api_key() },
        &["GOOGLE_GENERATIVE_AI_API_KEY"],
        |credential| credential.header("x-goog-api-key"),
    );
    Ok(language_model(
        model_id,
        ID,
        "gemini",
        Some("google"),
        Arc::new(Gemini),
        Endpoint {
            base_url: Some(settings.string("baseURL").unwrap_or(DEFAULT_BASE_URL).to_string()),
            path: PathPart::Default,
            query: StringMap::new(),
        },
        auth,
        settings.request_options(&["apiKey", "baseURL"]),
        None,
        None,
    ))
}

pub fn interactions_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let auth = api_key(
        ProviderAuthOptions { auth: None, api_key: settings.api_key() },
        &["GOOGLE_GENERATIVE_AI_API_KEY"],
        |credential| credential.header("x-goog-api-key"),
    );
    Ok(language_model(
        model_id,
        ID,
        google_interactions::ID,
        Some("google"),
        Arc::new(GoogleInteractions),
        Endpoint {
            base_url: Some(settings.string("baseURL").unwrap_or(google_interactions::DEFAULT_BASE_URL).to_string()),
            path: PathPart::Static(google_interactions::PATH.to_string()),
            query: StringMap::new(),
        },
        auth,
        settings.request_options(&["apiKey", "baseURL"]),
        None,
        None,
    ))
}
