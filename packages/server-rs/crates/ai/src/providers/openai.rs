use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::Env;
use crate::protocols::openai_chat;
use crate::protocols::openai_responses;
use crate::providers::open_responses_options;
use crate::providers::openai_options;
use crate::providers::{RuntimeModel, Settings, language_model};
use crate::route::auth::{Auth, ProviderAuthOptions, api_key};
use crate::route::endpoint::{Endpoint, PathPart};
use crate::schema::{AiError, RequestOptions, StringMap, merge_request_options};

const ID: &str = "openai";

pub fn model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let defaults = with_provider_options(open_responses_options::defaults());
    let model_defaults = openai_options::open_ai_default_options(model_id).map(with_provider_options);
    let settings_options = request_options(settings);
    let options = merge_request_options(&[Some(&defaults), model_defaults.as_ref(), settings_options.as_ref()]);
    Ok(language_model(
        model_id,
        ID,
        "openai-responses",
        Some("openai"),
        Arc::new(openai_responses::OpenAiResponses),
        endpoint(settings.string("baseURL").unwrap_or(openai_responses::DEFAULT_BASE_URL), openai_responses::PATH),
        auth(settings),
        options,
        None,
        None,
    ))
}

pub fn chat_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let model_defaults = openai_options::open_ai_default_options(model_id).map(with_provider_options);
    let settings_options = request_options(settings);
    let options = merge_request_options(&[model_defaults.as_ref(), settings_options.as_ref()]);
    Ok(language_model(
        model_id,
        ID,
        "openai-chat",
        Some("openai"),
        Arc::new(openai_chat::OpenAiChat),
        endpoint(settings.string("baseURL").unwrap_or(openai_chat::DEFAULT_BASE_URL), openai_chat::PATH),
        auth(settings),
        options,
        None,
        Some(json!({ "promptCacheKey": true })),
    ))
}

fn auth(settings: &Settings) -> Auth {
    api_key(ProviderAuthOptions { api_key: settings.api_key(), auth: None }, &["OPENAI_API_KEY"], |credential| {
        credential.bearer()
    })
}

fn endpoint(base_url: &str, path: &str) -> Endpoint {
    Endpoint { base_url: Some(base_url.to_string()), path: PathPart::Static(path.to_string()), query: StringMap::new() }
}

fn with_provider_options(options: Map<String, Value>) -> RequestOptions {
    RequestOptions { provider_options: Some(options), ..RequestOptions::default() }
}

fn request_options(settings: &Settings) -> Option<RequestOptions> {
    let options = settings.request_options(&["apiKey", "baseURL", "organization", "project"]);
    let mut headers = StringMap::new();
    if let Some(organization) = settings.string("organization") {
        headers.insert("OpenAI-Organization".to_string(), organization.to_string());
    }
    if let Some(project) = settings.string("project") {
        headers.insert("OpenAI-Project".to_string(), project.to_string());
    }
    if let Some(existing) = options.as_ref().and_then(|options| options.headers.as_ref()) {
        for (name, value) in existing.iter() {
            headers.insert(name.to_string(), value.to_string());
        }
    }
    let merged = RequestOptions { headers: (!headers.is_empty()).then_some(headers), ..options.unwrap_or_default() };
    (merged != RequestOptions::default()).then_some(merged)
}
