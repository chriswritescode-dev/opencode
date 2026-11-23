use std::sync::Arc;

use crate::Env;
use crate::protocols::openai_chat::OpenAiChat;
use crate::providers::{RuntimeModel, Settings, language_model};
use crate::route::auth::{ProviderAuthOptions, api_key};
use crate::route::endpoint::{Endpoint, PathPart};
use crate::schema::{AiError, AiErrorReason, StringMap};

const ID: &str = "openai-compatible";
const ROUTE_ID: &str = "openai-compatible-chat";
const PATH: &str = "/chat/completions";

pub fn model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let provider = settings.string("provider").unwrap_or(ID);
    let Some(base_url) = settings.string("baseURL") else {
        return Err(missing_base_url());
    };
    let auth =
        api_key(ProviderAuthOptions { api_key: settings.api_key(), auth: None }, &[], |credential| credential.bearer());
    Ok(language_model(
        model_id,
        provider,
        ROUTE_ID,
        Some(provider),
        Arc::new(OpenAiChat),
        Endpoint {
            base_url: Some(base_url.to_string()),
            path: PathPart::Static(PATH.to_string()),
            query: StringMap::new(),
        },
        auth,
        settings.request_options(&["apiKey", "baseURL", "provider"]),
        None,
        None,
    ))
}

fn missing_base_url() -> AiError {
    AiError {
        reason: AiErrorReason::InvalidRequest {
            message: "OpenAI-compatible providers require a baseURL".to_string(),
            body: None,
            http: None,
            parameter: None,
            classification: None,
        },
    }
}
