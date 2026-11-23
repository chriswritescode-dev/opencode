use std::sync::Arc;

use crate::Env;
use crate::protocols::open_responses::PATH as RESPONSES_PATH;
use crate::protocols::xai_responses::XaiResponses;
use crate::providers::{RuntimeModel, Settings, language_model_with_route, open_responses_options};
use crate::route::auth::{Auth, ProviderAuthOptions, api_key};
use crate::route::endpoint::Endpoint;
use crate::route::model::{Route, RouteTransport};
use crate::schema::{AiError, RequestOptions, merge_request_options};

pub const ID: &str = "xai";

const METADATA_KEY: &str = "xai";
const BASE_URL: &str = "https://api.x.ai/v1";
const DECLARED: [&str; 2] = ["apiKey", "baseURL"];

pub fn model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let route = Route {
        id: "xai-responses".to_string(),
        protocol: Arc::new(XaiResponses),
        provider_metadata_key: Some(METADATA_KEY.to_string()),
        endpoint: Endpoint::new(base_url(settings), RESPONSES_PATH),
        auth: auth(settings),
        headers: None,
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options: responses_options(settings),
    };
    Ok(language_model_with_route(model_id, ID, route, None, None))
}

fn base_url(settings: &Settings) -> &str {
    settings.string("baseURL").unwrap_or(BASE_URL)
}

fn auth(settings: &Settings) -> Auth {
    api_key(ProviderAuthOptions { auth: None, api_key: settings.api_key() }, &["XAI_API_KEY"], |credential| {
        credential.bearer()
    })
}

fn responses_options(settings: &Settings) -> Option<RequestOptions> {
    let defaults =
        RequestOptions { provider_options: Some(open_responses_options::defaults()), ..RequestOptions::default() };
    let settings_options = settings.request_options(&DECLARED);
    merge_request_options(&[Some(&defaults), settings_options.as_ref()])
}
