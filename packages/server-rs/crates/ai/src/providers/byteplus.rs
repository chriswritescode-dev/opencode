use crate::Env;
use crate::media::route::MediaEndpoint;
use crate::modalities::video::VideoModel;
use crate::protocols::seedance_video;
use crate::providers::{RuntimeModel, Settings, modality_options};
use crate::route::auth::{Auth, ProviderAuthOptions, bearer};
use crate::schema::AiError;

pub const ID: &str = "byteplus";

const DECLARED: [&str; 2] = ["apiKey", "baseURL"];

pub fn video_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = modality_options(settings, &DECLARED);
    let endpoint = MediaEndpoint::path(seedance_video::PATH)
        .with_base_url(settings.string("baseURL").unwrap_or(seedance_video::BYTEPLUS_BASE_URL));
    Ok(RuntimeModel::Video(VideoModel::queued(
        model_id,
        ID.to_string(),
        seedance_video::protocol(ID),
        endpoint,
        auth(settings),
        None,
        transport,
        provider_options,
    )))
}

fn auth(settings: &Settings) -> Auth {
    bearer(ProviderAuthOptions { auth: None, api_key: settings.api_key() }, &["BYTEPLUS_API_KEY"])
}
