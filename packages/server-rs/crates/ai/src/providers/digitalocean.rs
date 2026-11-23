use std::sync::Arc;

use serde_json::{Value, json};

use crate::Env;
use crate::ExecutionContext;
use crate::protocols::open_responses::{OpenResponses, PATH as RESPONSES_PATH};
use crate::protocols::openai_chat::{self, OpenAiChat, PATH as CHAT_PATH};
use crate::protocols::utils::cache::CacheControl;
use crate::providers::{RuntimeModel, Settings, language_model_with_route, open_responses_options};
use crate::route::auth::{Auth, ProviderAuthOptions, api_key};
use crate::route::endpoint::Endpoint;
use crate::route::framing::Definition;
use crate::route::model::{Model, ResolvedRequest, Route, RouteTransport};
use crate::route::protocol::{Protocol, ProtocolState};
use crate::schema::{AiError, RequestOptions, merge_request_options};

pub const ID: &str = "digitalocean";

const BASE_URL: &str = "https://inference.do-ai.run/v1";
const CHAT_ID: &str = "digitalocean";
const RESPONSES_ID: &str = "digitalocean-responses";
const METADATA_KEY: &str = "digitalocean";
const AUTH_ENV_VARS: [&str; 3] = ["DIGITALOCEAN_ACCESS_TOKEN", "DIGITALOCEAN_API_KEY", "DO_INFERENCE_API_KEY"];
const DECLARED: [&str; 2] = ["apiKey", "baseURL"];

pub struct DigitalOceanChat;

impl Protocol for DigitalOceanChat {
    fn id(&self) -> &str {
        "digitalocean-chat"
    }

    fn build_body(
        &self,
        request: &ResolvedRequest,
        model: &Model,
        _context: &ExecutionContext,
    ) -> Result<Value, AiError> {
        openai_chat::from_request_with(&request.request, model, Some(&mut CacheControl::new()))
    }

    fn default_path(&self, model: &Model, body: &Value) -> Option<String> {
        OpenAiChat.default_path(model, body)
    }

    fn framing(&self) -> Definition {
        OpenAiChat.framing()
    }

    fn new_state(&self, request: &ResolvedRequest, context: &ExecutionContext) -> Box<dyn ProtocolState> {
        OpenAiChat.new_state(request, context)
    }
}

pub fn model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let route = Route {
        id: CHAT_ID.to_string(),
        protocol: Arc::new(DigitalOceanChat),
        provider_metadata_key: Some(METADATA_KEY.to_string()),
        endpoint: endpoint(settings, CHAT_PATH),
        auth: auth(settings),
        headers: None,
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options: settings.request_options(&DECLARED),
    };
    Ok(language_model_with_route(model_id, ID, route, None, Some(json!({ "promptCacheKey": true }))))
}

pub fn responses_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let base =
        RequestOptions { provider_options: Some(open_responses_options::defaults()), ..RequestOptions::default() };
    let settings_options = settings.request_options(&DECLARED);
    let options = merge_request_options(&[Some(&base), settings_options.as_ref()]);
    let route = Route {
        id: RESPONSES_ID.to_string(),
        protocol: Arc::new(OpenResponses),
        provider_metadata_key: Some(METADATA_KEY.to_string()),
        endpoint: endpoint(settings, RESPONSES_PATH),
        auth: auth(settings),
        headers: None,
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options,
    };
    Ok(language_model_with_route(model_id, ID, route, None, None))
}

fn auth(settings: &Settings) -> Auth {
    api_key(ProviderAuthOptions { auth: None, api_key: settings.api_key() }, &AUTH_ENV_VARS, |credential| {
        credential.bearer()
    })
}

fn endpoint(settings: &Settings, path: &str) -> Endpoint {
    Endpoint::new(settings.string("baseURL").unwrap_or(BASE_URL), path)
}
