use std::sync::Arc;

use crate::Env;
use crate::media::route::MediaEndpoint;
use crate::modalities::image::ImageModel;
use crate::modalities::video::VideoModel;
use crate::protocols::open_responses::PATH as RESPONSES_PATH;
use crate::protocols::xai_images;
use crate::protocols::xai_responses::XaiResponses;
use crate::protocols::xai_video;
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

pub fn image_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = crate::providers::modality_options(settings, &DECLARED);
    let endpoint = MediaEndpoint::function(std::sync::Arc::new(|request: &crate::modalities::image::ImageRequest| {
        xai_images::path(!request.images.is_empty())
    }))
    .with_base_url(base_url(settings));
    let route = ImageModel::inline(
        model_id,
        ID.to_string(),
        xai_images::protocol(),
        endpoint,
        auth(settings),
        None,
        transport,
        provider_options,
    );
    Ok(RuntimeModel::Image(route))
}

pub fn video_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = crate::providers::modality_options(settings, &DECLARED);
    let endpoint =
        MediaEndpoint::function(Arc::new(|request: &crate::modalities::video::VideoRequest| xai_video::path(request)))
            .with_base_url(base_url(settings));
    Ok(RuntimeModel::Video(VideoModel::queued(
        model_id,
        ID.to_string(),
        xai_video::protocol(),
        endpoint,
        auth(settings),
        None,
        transport,
        provider_options,
    )))
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
