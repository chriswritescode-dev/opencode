use std::sync::Arc;

use crate::Env;
use crate::media::route::MediaEndpoint;
use crate::modalities::image::ImageModel;
use crate::protocols::zai_chat::{self, ZaiChat};
use crate::protocols::zai_images;
use crate::providers::{RuntimeModel, Settings, language_model, modality_options};
use crate::route::auth::{ProviderAuthOptions, bearer};
use crate::route::endpoint::Endpoint;
use crate::schema::AiError;

pub const ID: &str = "zai";

const BASE_URL: &str = "https://api.z.ai/api/paas/v4";
const PATH: &str = "/chat/completions";

pub fn model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let base_url = settings.string("baseURL").unwrap_or(BASE_URL);
    let auth = bearer(ProviderAuthOptions { auth: None, api_key: settings.api_key() }, &["ZAI_API_KEY"]);
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

pub fn image_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = modality_options(settings, &["apiKey", "baseURL"]);
    let auth = bearer(ProviderAuthOptions { auth: None, api_key: settings.api_key() }, &["ZAI_API_KEY"]);
    let endpoint = MediaEndpoint::path(zai_images::PATH)
        .with_base_url(settings.string("baseURL").unwrap_or(zai_images::DEFAULT_BASE_URL));
    let route = ImageModel::inline(
        model_id,
        ID.to_string(),
        zai_images::protocol(),
        endpoint,
        auth,
        None,
        transport,
        provider_options,
    );
    Ok(RuntimeModel::Image(route))
}
