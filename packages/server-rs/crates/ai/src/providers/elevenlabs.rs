use std::sync::Arc;

use crate::Env;
use crate::media::route::MediaEndpoint;
use crate::modalities::speech::SpeechModel;
use crate::protocols::elevenlabs_speech;
use crate::providers::{RuntimeModel, Settings, modality_options};
use crate::route::auth::{ProviderAuthOptions, api_key};
use crate::schema::AiError;

pub const ID: &str = "elevenlabs";

pub fn speech_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = modality_options(settings, &["apiKey", "baseURL"]);
    let endpoint = MediaEndpoint::function(Arc::new(elevenlabs_speech::path))
        .with_base_url(settings.string("baseURL").unwrap_or(elevenlabs_speech::DEFAULT_BASE_URL));
    Ok(RuntimeModel::Speech(SpeechModel::streamed(
        model_id,
        ID.to_string(),
        Arc::new(elevenlabs_speech::protocol()),
        endpoint,
        api_key(
            ProviderAuthOptions { api_key: settings.api_key(), auth: None },
            &["ELEVENLABS_API_KEY"],
            |credential| credential.header("xi-api-key"),
        ),
        None,
        transport,
        provider_options,
    )))
}
