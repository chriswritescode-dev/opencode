use std::sync::Arc;

use serde_json::json;

use crate::Env;
use crate::protocols::openai_chat::OpenAiChat;
use crate::providers::{RuntimeModel, Settings, language_model};
use crate::route::auth::{Auth, ProviderAuthOptions, bearer};
use crate::route::endpoint::Endpoint;
use crate::schema::AiError;

pub const ID: &str = "deepseek";
const BASE_URL: &str = "https://api.deepseek.com/v1";
const PATH: &str = "/chat/completions";
const ROUTE_ID: &str = "deepseek-chat";
const METADATA_KEY: &str = "deepseek";
const DECLARED: [&str; 2] = ["apiKey", "baseURL"];

pub fn model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    Ok(language_model(
        model_id,
        ID,
        ROUTE_ID,
        Some(METADATA_KEY),
        Arc::new(OpenAiChat),
        Endpoint::new(settings.string("baseURL").unwrap_or(BASE_URL), PATH),
        auth(settings),
        settings.request_options(&DECLARED),
        Some(json!({ "maxTokensField": "max_tokens" })),
        None,
    ))
}

fn auth(settings: &Settings) -> Auth {
    bearer(ProviderAuthOptions { auth: None, api_key: settings.api_key() }, &["DEEPSEEK_API_KEY"])
}
