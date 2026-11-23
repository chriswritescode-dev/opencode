use std::sync::Arc;

use crate::Env;
use crate::protocols::open_responses::OpenResponses;
use crate::protocols::venice_chat::{self, VeniceChat};
use crate::providers::{RuntimeModel, Settings, language_model, open_responses_options};
use crate::route::auth::{ProviderAuthOptions, api_key};
use crate::route::endpoint::{Endpoint, PathPart};
use crate::schema::{AiError, RequestOptions, StringMap, merge_request_options};

pub const ID: &str = "venice";
const BASE_URL: &str = "https://api.venice.ai/api/v1";

pub fn model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let base_url = settings.string("baseURL").unwrap_or(BASE_URL);
    let auth =
        api_key(ProviderAuthOptions { auth: None, api_key: settings.api_key() }, &["VENICE_API_KEY"], |credential| {
            credential.bearer()
        });
    Ok(language_model(
        model_id,
        ID,
        venice_chat::ID,
        Some("venice"),
        Arc::new(VeniceChat),
        Endpoint::new(base_url, venice_chat::PATH),
        auth,
        settings.request_options(&["apiKey", "baseURL"]),
        Some(venice_chat::compatibility()),
        Some(venice_chat::capabilities()),
    ))
}

pub fn responses_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let base_url = settings.string("baseURL").unwrap_or(BASE_URL);
    let auth =
        api_key(ProviderAuthOptions { auth: None, api_key: settings.api_key() }, &["VENICE_API_KEY"], |credential| {
            credential.bearer()
        });
    let base =
        RequestOptions { provider_options: Some(open_responses_options::defaults()), ..RequestOptions::default() };
    let extra = settings.request_options(&["apiKey", "baseURL"]);
    let options = merge_request_options(&[Some(&base), extra.as_ref()]);
    Ok(language_model(
        model_id,
        ID,
        "venice-responses",
        Some("venice"),
        Arc::new(OpenResponses),
        Endpoint {
            base_url: Some(base_url.to_string()),
            path: PathPart::Static(crate::protocols::open_responses::PATH.to_string()),
            query: StringMap::new(),
        },
        auth,
        options,
        None,
        None,
    ))
}
