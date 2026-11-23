use std::sync::Arc;

use crate::Env;
use crate::protocols::zai_chat::{self, ZaiChat};
use crate::providers::{RuntimeModel, Settings, language_model};
use crate::route::auth::{ProviderAuthOptions, api_key};
use crate::route::endpoint::Endpoint;
use crate::schema::AiError;

pub const ID: &str = "zai";

const BASE_URL: &str = "https://api.z.ai/api/paas/v4";
const PATH: &str = "/chat/completions";

pub fn model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let base_url = settings.string("baseURL").unwrap_or(BASE_URL);
    let auth =
        api_key(ProviderAuthOptions { auth: None, api_key: settings.api_key() }, &["ZAI_API_KEY"], |credential| {
            credential.bearer()
        });
    Ok(language_model(
        model_id,
        ID,
        "zai-chat",
        Some("zai"),
        Arc::new(ZaiChat),
        Endpoint::new(base_url, PATH),
        auth,
        settings.request_options(&["apiKey", "baseURL"]),
        Some(zai_chat::compatibility()),
        Some(zai_chat::capabilities()),
    ))
}

pub fn chat_model(model_id: &str, settings: &Settings, env: &dyn Env) -> Result<RuntimeModel, AiError> {
    model(model_id, settings, env)
}
