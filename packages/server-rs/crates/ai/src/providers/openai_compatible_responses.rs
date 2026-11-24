use std::sync::Arc;

use crate::Env;
use crate::protocols::open_responses;
use crate::providers::open_responses_options;
use crate::providers::{RuntimeModel, Settings, language_model};
use crate::route::auth::{ProviderAuthOptions, bearer};
use crate::route::endpoint::{Endpoint, PathPart};
use crate::schema::{AiError, AiErrorReason, RequestOptions, StringMap, merge_request_options};

const ID: &str = "openai-compatible";
const ROUTE_ID: &str = "openai-compatible-responses";

pub fn model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let provider = settings.string("provider").unwrap_or(ID);
    let Some(base_url) = settings.string("baseURL") else {
        return Err(missing_base_url());
    };
    let auth = bearer(ProviderAuthOptions { api_key: settings.api_key(), auth: None }, &[]);
    let defaults =
        RequestOptions { provider_options: Some(open_responses_options::defaults()), ..RequestOptions::default() };
    let settings_options = settings.request_options(&["apiKey", "baseURL", "provider"]);
    let options = merge_request_options(&[Some(&defaults), settings_options.as_ref()]);
    Ok(language_model(
        model_id,
        provider,
        ROUTE_ID,
        Some(provider),
        Arc::new(open_responses::OpenResponses),
        Endpoint {
            base_url: Some(base_url.to_string()),
            path: PathPart::Static(open_responses::PATH.to_string()),
            query: StringMap::new(),
        },
        auth,
        options,
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
