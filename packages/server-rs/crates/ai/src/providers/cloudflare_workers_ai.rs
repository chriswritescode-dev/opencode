use std::sync::Arc;

use crate::Env;
use crate::protocols::openai_chat;
use crate::providers::cloudflare_shared;
use crate::providers::{RuntimeModel, Settings, language_model_with_route};
use crate::route::auth::{ProviderAuthOptions, api_key};
use crate::route::endpoint::{Endpoint, PathPart};
use crate::route::model::{Route, RouteTransport};
use crate::schema::{AiError, StringMap};

pub const ID: &str = "cloudflare-workers-ai";

const AUTH_ENV_VARS: [&str; 3] = ["CLOUDFLARE_API_KEY", "CLOUDFLARE_WORKERS_AI_TOKEN", "CLOUDFLARE_API_TOKEN"];

pub fn model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let auth = api_key(ProviderAuthOptions { auth: None, api_key: settings.api_key() }, &AUTH_ENV_VARS, |credential| {
        credential.bearer()
    });
    let route = Route {
        id: ID.to_string(),
        protocol: Arc::new(openai_chat::OpenAiChat),
        provider_metadata_key: Some(ID.to_string()),
        endpoint: Endpoint {
            base_url: Some(cloudflare_shared::base_url(settings, "CloudflareWorkersAI")?),
            path: PathPart::Static(openai_chat::PATH.to_string()),
            query: StringMap::new(),
        },
        auth,
        headers: None,
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options: settings.request_options(&["accountId", "apiKey", "baseURL", "body"]),
    };
    Ok(language_model_with_route(model_id, ID, route, None, None))
}
