use std::sync::Arc;

use serde_json::json;

use crate::Env;
use crate::protocols::openai_chat::OpenAiChat;
use crate::providers::{RuntimeModel, Settings, language_model};
use crate::route::auth::{ProviderAuthOptions, api_key};
use crate::route::endpoint::Endpoint;
use crate::schema::AiError;

pub const ID: &str = "deepinfra";
const BASE_URL: &str = "https://api.deepinfra.com/v1/openai";
const PATH: &str = "/chat/completions";

pub fn model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let base_url = settings.string("baseURL").map_or_else(
        || BASE_URL.to_string(),
        |root| {
            let root = root.trim_end_matches('/');
            if root.ends_with("/openai") { root.to_string() } else { format!("{root}/openai") }
        },
    );
    let auth = api_key(
        ProviderAuthOptions { auth: None, api_key: settings.api_key() },
        &["DEEPINFRA_API_KEY"],
        |credential| credential.bearer(),
    );
    Ok(language_model(
        model_id,
        ID,
        "deepinfra-chat",
        Some("deepinfra"),
        Arc::new(OpenAiChat),
        Endpoint::new(base_url, PATH),
        auth,
        settings.request_options(&["apiKey", "baseURL"]),
        Some(json!({ "maxTokensField": "max_tokens", "reasoningField": "reasoning_content" })),
        Some(json!({ "promptCacheKey": true })),
    ))
}
