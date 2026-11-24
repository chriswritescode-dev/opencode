use std::sync::Arc;

use crate::Env;
use crate::media::route::MediaEndpoint;
use crate::modalities::image::ImageModel;
use crate::protocols::cloudflare_run_decisions::{self, CloudflareRunDecisions};
use crate::protocols::cloudflare_workers_ai_images;
use crate::protocols::openai_chat;
use crate::providers::cloudflare_shared;
use crate::providers::{RuntimeModel, Settings, language_model_with_route, modality_options};
use crate::route::auth::{Auth, ProviderAuthOptions, api_key};
use crate::route::decision::{DecisionRoute, DecisionRouteInput};
use crate::route::endpoint::{Endpoint, PathPart};
use crate::route::model::{Route, RouteTransport};
use crate::schema::{AiError, StringMap};

pub const ID: &str = "cloudflare-workers-ai";

const AUTH_ENV_VARS: [&str; 3] = ["CLOUDFLARE_API_KEY", "CLOUDFLARE_WORKERS_AI_TOKEN", "CLOUDFLARE_API_TOKEN"];
const DECLARED: [&str; 4] = ["accountId", "apiKey", "baseURL", "body"];

pub fn model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let route = Route {
        id: ID.to_string(),
        protocol: Arc::new(openai_chat::OpenAiChat),
        provider_metadata_key: Some(ID.to_string()),
        endpoint: Endpoint {
            base_url: Some(cloudflare_shared::base_url(settings, "CloudflareWorkersAI")?),
            path: PathPart::Static(openai_chat::PATH.to_string()),
            query: StringMap::new(),
        },
        auth: auth(settings),
        headers: None,
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options: settings.request_options(&DECLARED),
    };
    Ok(language_model_with_route(model_id, ID, route, None, None))
}

pub fn decision_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = modality_options(settings, &DECLARED);
    let route = DecisionRoute::make(DecisionRouteInput {
        id: cloudflare_run_decisions::ID.to_string(),
        provider: ID.to_string(),
        protocol: Arc::new(CloudflareRunDecisions),
        endpoint: MediaEndpoint::path("")
            .with_base_url(cloudflare_shared::run_base_url(settings, "CloudflareWorkersAI")?),
        auth: auth(settings),
        headers: None,
        provider_metadata_key: Some(ID.to_string()),
        options: transport,
    });
    Ok(RuntimeModel::Decision(route.model(model_id, None, provider_options)))
}

pub fn image_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = modality_options(settings, &DECLARED);
    let endpoint = MediaEndpoint::function(std::sync::Arc::new(|request: &crate::modalities::image::ImageRequest| {
        format!("/{}", request.model.id)
    }))
    .with_base_url(cloudflare_shared::run_base_url(settings, "CloudflareWorkersAI")?);
    let route = ImageModel::inline(
        model_id,
        ID.to_string(),
        cloudflare_workers_ai_images::protocol(),
        endpoint,
        auth(settings),
        None,
        transport,
        provider_options,
    );
    Ok(RuntimeModel::Image(route))
}

fn auth(settings: &Settings) -> Auth {
    api_key(ProviderAuthOptions { auth: None, api_key: settings.api_key() }, &AUTH_ENV_VARS, |credential| {
        credential.bearer()
    })
}
