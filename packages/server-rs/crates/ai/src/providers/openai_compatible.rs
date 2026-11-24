use std::sync::Arc;

use serde_json::json;

use crate::Env;
use crate::media::route::MediaEndpoint;
use crate::protocols::openai_chat::OpenAiChat;
use crate::protocols::system_one::{self, SystemOne};
use crate::providers::{RuntimeModel, Settings, language_model, modality_options};
use crate::route::auth::{Auth, ProviderAuthOptions, api_key};
use crate::route::decision::{DecisionRoute, DecisionRouteInput};
use crate::route::endpoint::{Endpoint, PathPart};
use crate::schema::{AiError, AiErrorReason, StringMap};

const ID: &str = "openai-compatible";
const ROUTE_ID: &str = "openai-compatible-chat";
const PATH: &str = "/chat/completions";
const DECLARED: [&str; 3] = ["apiKey", "baseURL", "provider"];

pub fn model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let provider = settings.string("provider").unwrap_or(ID);
    let Some(base_url) = settings.string("baseURL") else {
        return Err(missing_base_url());
    };
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
        auth(settings),
        settings.request_options(&DECLARED),
        None,
        None,
    ))
}

pub fn decision_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let provider = settings.string("provider").unwrap_or(ID);
    let Some(base_url) = settings.string("baseURL") else {
        return Err(missing_base_url());
    };
    let (transport, provider_options) = modality_options(settings, &DECLARED);
    let route = DecisionRoute::make(DecisionRouteInput {
        id: system_one::ID.to_string(),
        provider: provider.to_string(),
        protocol: Arc::new(SystemOne),
        endpoint: MediaEndpoint::path(system_one::PATH).with_base_url(base_url),
        auth: auth(settings),
        headers: None,
        provider_metadata_key: Some(provider.to_string()),
        options: transport,
    });
    Ok(RuntimeModel::Decision(route.model(model_id, Some(json!({ "mediaInput": "root" })), provider_options)))
}

fn auth(settings: &Settings) -> Auth {
    api_key(ProviderAuthOptions { api_key: settings.api_key(), auth: None }, &[], |credential| credential.bearer())
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
