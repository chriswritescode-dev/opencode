use crate::Env;
use crate::media::route::MediaEndpoint;
use crate::modalities::transcription::TranscriptionModel;
use crate::protocols::assemblyai_transcription;
use crate::providers::{RuntimeModel, Settings, modality_options};
use crate::route::auth::{ProviderAuthOptions, api_key};
use crate::schema::AiError;

pub const ID: &str = "assemblyai";

pub fn transcription_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = modality_options(settings, &["apiKey", "baseURL"]);
    let endpoint = MediaEndpoint::path(assemblyai_transcription::PATH)
        .with_base_url(settings.string("baseURL").unwrap_or(assemblyai_transcription::DEFAULT_BASE_URL));
    Ok(RuntimeModel::Transcription(TranscriptionModel::queued(
        model_id,
        ID.to_string(),
        assemblyai_transcription::protocol(),
        endpoint,
        api_key(
            ProviderAuthOptions { api_key: settings.api_key(), auth: None },
            &["ASSEMBLYAI_API_KEY"],
            |credential| credential.header("authorization"),
        ),
        None,
        transport,
        provider_options,
    )))
}
