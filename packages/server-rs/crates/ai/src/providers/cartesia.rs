use std::sync::Arc;

use crate::Env;
use crate::media::route::MediaEndpoint;
use crate::modalities::speech::SpeechModel;
use crate::protocols::cartesia_speech;
use crate::providers::{RuntimeModel, Settings, modality_options};
use crate::route::auth::{ProviderAuthOptions, bearer};
use crate::schema::{AiError, StringMap};

pub const ID: &str = "cartesia";

pub fn speech_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = modality_options(settings, &["apiKey", "baseURL"]);
    let endpoint = MediaEndpoint::function(Arc::new(cartesia_speech::path))
        .with_base_url(settings.string("baseURL").unwrap_or(cartesia_speech::DEFAULT_BASE_URL));
    let headers = StringMap::from_pairs([("Cartesia-Version".to_string(), cartesia_speech::API_VERSION.to_string())]);
    Ok(RuntimeModel::Speech(SpeechModel::streamed(
        model_id,
        ID.to_string(),
        Arc::new(cartesia_speech::protocol()),
        endpoint,
        bearer(ProviderAuthOptions { api_key: settings.api_key(), auth: None }, &["CARTESIA_API_KEY"]),
        Some(headers),
        transport,
        provider_options,
    )))
}
