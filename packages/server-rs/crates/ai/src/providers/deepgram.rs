use std::sync::Arc;

use crate::Env;
use crate::media::route::MediaEndpoint;
use crate::modalities::speech::SpeechModel;
use crate::modalities::transcription::TranscriptionModel;
use crate::protocols::deepgram_speech;
use crate::protocols::deepgram_transcription;
use crate::providers::{RuntimeModel, Settings, modality_options};
use crate::route::auth::{ProviderAuthOptions, api_key};
use crate::schema::AiError;

pub const ID: &str = "deepgram";

pub fn speech_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = modality_options(settings, &["apiKey", "baseURL"]);
    let endpoint = MediaEndpoint::path(deepgram_speech::PATH)
        .with_base_url(settings.string("baseURL").unwrap_or(deepgram_speech::DEFAULT_BASE_URL));
    Ok(RuntimeModel::Speech(SpeechModel::streamed(
        model_id,
        ID.to_string(),
        Arc::new(deepgram_speech::protocol()),
        endpoint,
        api_key(ProviderAuthOptions { api_key: settings.api_key(), auth: None }, &["DEEPGRAM_API_KEY"], |credential| {
            credential.scheme("Token")
        }),
        None,
        transport,
        provider_options,
    )))
}

pub fn transcription_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = modality_options(settings, &["apiKey", "baseURL"]);
    let endpoint = MediaEndpoint::path(deepgram_transcription::PATH)
        .with_base_url(settings.string("baseURL").unwrap_or(deepgram_transcription::DEFAULT_BASE_URL));
    Ok(RuntimeModel::Transcription(TranscriptionModel::inline(
        model_id,
        ID.to_string(),
        deepgram_transcription::protocol(),
        endpoint,
        api_key(ProviderAuthOptions { api_key: settings.api_key(), auth: None }, &["DEEPGRAM_API_KEY"], |credential| {
            credential.scheme("Token")
        }),
        None,
        transport,
        provider_options,
    )))
}
