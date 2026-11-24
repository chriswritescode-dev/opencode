use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

use crate::media::generation::{Reason, Snapshot};
use crate::media::protocol::{Body, DecodeContext, Failure, Identity, State, TextResponse, state, status};
use crate::media::route::{MediaPrepare, MediaSend, PollContext, QueuedProtocol, Started};
use crate::modalities::transcription::{
    MediaUsage, TranscriptionRequest, TranscriptionResponse, TranscriptionSegment, TranscriptionWord,
};
use crate::protocols::shared;
use crate::protocols::utils::media_input;
use crate::route::executor::BoxFuture;
use crate::schema::{AiError, MediaAsset, MediaSource, ProviderId, ProviderMetadata};

pub const ID: &str = "assemblyai-transcription";
pub const NAME: &str = "AssemblyAI";
pub const DEFAULT_BASE_URL: &str = "https://api.assemblyai.com";
pub const PATH: &str = "/v2/transcript";
pub const UPLOAD_PATH: &str = "/v2/upload";

const PROVIDER: &str = "assemblyai";

const STATUS: &[(&str, State)] = &[
    ("queued", State::Running),
    ("processing", State::Running),
    ("completed", State::Completed),
    ("error", State::Failed),
];

#[derive(Clone, Serialize, Deserialize)]
pub struct Token {
    #[serde(rename = "transcriptID")]
    pub transcript_id: String,
}

pub struct AssemblyAiTranscription {
    provider: ProviderId,
}

pub fn protocol() -> Arc<dyn QueuedProtocol<TranscriptionRequest, TranscriptionResponse, Token>> {
    Arc::new(AssemblyAiTranscription { provider: ProviderId::from(PROVIDER) })
}

#[derive(Deserialize)]
struct Upload {
    upload_url: String,
}

#[derive(Deserialize)]
struct Transcript {
    id: String,
    status: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    words: Option<Vec<Word>>,
    #[serde(default)]
    utterances: Option<Vec<Utterance>>,
    #[serde(default)]
    language_code: Option<String>,
    #[serde(default)]
    audio_duration: Option<Number>,
    #[serde(default)]
    speech_model_used: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize)]
struct Word {
    text: String,
    start: Number,
    end: Number,
    #[serde(default)]
    confidence: Option<Number>,
    #[serde(default)]
    speaker: Option<String>,
}

#[derive(Deserialize)]
struct Utterance {
    text: String,
    start: Number,
    end: Number,
    #[serde(default)]
    speaker: Option<String>,
}

impl AssemblyAiTranscription {
    fn identity(&self) -> Identity {
        Identity { id: ID.to_string(), name: NAME.to_string(), provider: self.provider.clone() }
    }
}

impl QueuedProtocol<TranscriptionRequest, TranscriptionResponse, Token> for AssemblyAiTranscription {
    fn id(&self) -> &str {
        ID
    }

    fn provider(&self) -> &ProviderId {
        &self.provider
    }

    fn token_decode(&self, value: Value) -> Result<Token, AiError> {
        serde_json::from_value(value)
            .map_err(|_| self.identity().invalid(format!("{NAME} returned an invalid token"), None, None))
    }

    fn token_encode(&self, token: &Token) -> Value {
        serde_json::to_value(token).unwrap_or(Value::Null)
    }

    fn body_from(&self, request: &TranscriptionRequest) -> Result<Body, AiError> {
        let audio =
            media_input::media_reference(&request.audio, Some(PROVIDER), NAME).map_err(shared::invalid_request)?;
        let mut base = Map::new();
        base.insert("audio_url".to_string(), Value::String(audio.value()));
        base.insert("speech_models".to_string(), Value::Array(vec![Value::String(request.model.id.clone())]));
        insert(&mut base, "language_code", request.language.clone().map(Value::String));
        if request.language.is_none() {
            base.insert("language_detection".to_string(), Value::Bool(true));
        }
        insert(&mut base, "prompt", request.prompt.clone().map(Value::String));
        if request.diarize == Some(true)
            || request.timestamps.as_deref() == Some("segment")
            || request.speakers.is_some()
        {
            base.insert("speaker_labels".to_string(), Value::Bool(true));
        }
        insert(&mut base, "speakers_expected", request.speakers.map(|speakers| Value::Number(Number::from(speakers))));
        let options = request.provider_options.clone().unwrap_or_default();
        let http = request.http.as_ref().and_then(|http| http.body.clone()).unwrap_or_default();
        let value = crate::json::merge_records([&base, &options, &http]).unwrap_or_default();
        Ok(Body::json(value, None))
    }

    fn prepare(&self) -> Option<MediaPrepare<TranscriptionRequest>> {
        let identity = self.identity();
        let prepare: MediaPrepare<TranscriptionRequest> =
            Arc::new(move |request: TranscriptionRequest, send: MediaSend| {
                let identity = identity.clone();
                Box::pin(async move {
                    if !matches!(&request.audio.source, MediaSource::Bytes { .. } | MediaSource::Base64 { .. }) {
                        return Ok(request);
                    }
                    let bytes = media_input::inline_bytes(ID, &request.audio).map_err(shared::invalid_request)?;
                    let uploaded =
                        send(UPLOAD_PATH.to_string(), Body::binary(bytes, "application/octet-stream", None)).await?;
                    let upload: Upload = identity.decode_json(&uploaded)?;
                    let media_type = Some(shared::media_type(&request.audio));
                    let mut request = request;
                    request.audio = MediaAsset {
                        source: MediaSource::Url { url: upload.upload_url, media_type, expires_at: None },
                        info: None,
                        provider_metadata: None,
                        headers: None,
                    };
                    Ok(request)
                })
            });
        Some(prepare)
    }

    fn start_decode(
        &self,
        response: &TextResponse,
        _context: &DecodeContext<TranscriptionRequest>,
    ) -> Result<Started<Token>, AiError> {
        let value: Transcript = self.identity().decode_json(response)?;
        let mut snapshot = status(STATUS, &value.status);
        snapshot.id = value.id.clone();
        Ok(Started { token: Token { transcript_id: value.id }, snapshot })
    }

    fn status_path(&self, token: &Token) -> String {
        format!("{PATH}/{}", token.transcript_id)
    }

    fn status_decode(&self, response: &TextResponse, context: &PollContext<Token>) -> Result<Snapshot, AiError> {
        let value: Transcript = self.identity().decode_json(response)?;
        let mut snapshot = status(STATUS, &value.status);
        snapshot.id = context.token.transcript_id.clone();
        Ok(snapshot)
    }

    fn result_path(&self, token: &Token) -> String {
        format!("{PATH}/{}", token.transcript_id)
    }

    fn result_decode(
        &self,
        response: TextResponse,
        context: PollContext<Token>,
    ) -> BoxFuture<Result<TranscriptionResponse, AiError>> {
        Box::pin(async move {
            let output = identity().output(&response);
            let transcript: Transcript = output.json()?;
            let state = state(STATUS, &transcript.status);
            let error = transcript.error.as_deref();
            if state == State::Failed {
                let suffix = error.map(|error| format!(": {error}")).unwrap_or_default();
                return Err(output.ended(
                    Reason::Failed,
                    format!("{NAME} transcription failed{suffix}"),
                    Failure::ProviderInternal,
                ));
            }
            if state != State::Completed {
                return Err(output.pending(NAME, &context.token.transcript_id));
            }
            let segments = transcript.utterances.as_ref().map(|utterances| {
                utterances
                    .iter()
                    .map(|utterance| TranscriptionSegment {
                        text: utterance.text.clone(),
                        start_seconds: seconds(&utterance.start),
                        end_seconds: seconds(&utterance.end),
                        speaker: utterance.speaker.clone(),
                    })
                    .collect::<Vec<TranscriptionSegment>>()
            });
            let words = transcript.words.as_ref().map(|words| {
                words
                    .iter()
                    .map(|word| TranscriptionWord {
                        text: word.text.clone(),
                        start_seconds: seconds(&word.start),
                        end_seconds: seconds(&word.end),
                        speaker: word.speaker.clone(),
                        confidence: word.confidence.clone(),
                    })
                    .collect::<Vec<TranscriptionWord>>()
            });
            let duration = transcript.audio_duration.clone();
            let usage = duration.clone().map(|seconds| MediaUsage::Seconds { seconds });
            let mut assemblyai = Map::new();
            assemblyai.insert("transcriptId".to_string(), Value::String(transcript.id.clone()));
            if let Some(model) = &transcript.speech_model_used {
                assemblyai.insert("speechModel".to_string(), Value::String(model.clone()));
            }
            let mut provider_metadata = ProviderMetadata::new();
            provider_metadata.insert(PROVIDER.to_string(), Value::Object(assemblyai));
            Ok(TranscriptionResponse {
                text: transcript.text.unwrap_or_default(),
                segments,
                words,
                language: transcript.language_code.map(|language| language.to_lowercase()),
                duration_seconds: duration,
                usage,
                provider_metadata: Some(provider_metadata),
            })
        })
    }
}

fn seconds(milliseconds: &Number) -> Number {
    shared::number_from_f64(milliseconds.as_f64().unwrap_or_default() / 1000.0).unwrap_or_else(|| Number::from(0))
}

fn identity() -> Identity {
    Identity { id: ID.to_string(), name: NAME.to_string(), provider: ProviderId::from(PROVIDER) }
}

fn insert(target: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        target.insert(key.to_string(), value);
    }
}
