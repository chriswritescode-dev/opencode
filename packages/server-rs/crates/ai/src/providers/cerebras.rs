use std::sync::Arc;

use serde_json::json;

use crate::Env;
use crate::protocols::openai_chat::OpenAiChat;
use crate::providers::{RuntimeModel, Settings, language_model};
use crate::route::auth::{ProviderAuthOptions, bearer};
use crate::route::endpoint::Endpoint;
use crate::schema::AiError;

pub const ID: &str = "cerebras";
const BASE_URL: &str = "https://api.cerebras.ai/v1";
const PATH: &str = "/chat/completions";

pub fn model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let base_url = settings.string("baseURL").unwrap_or(BASE_URL);
    let auth = bearer(ProviderAuthOptions { auth: None, api_key: settings.api_key() }, &["CEREBRAS_API_KEY"]);
    Ok(language_model(
        model_id,
        ID,
        "cerebras-chat",
        Some("cerebras"),
        Arc::new(OpenAiChat),
        Endpoint::new(base_url, PATH),
        auth,
        settings.request_options(&["apiKey", "baseURL"]),
        Some(json!({ "maxTokensField": "max_tokens", "reasoningField": "reasoning" })),
        Some(json!({ "promptCacheKey": true })),
    ))
}
