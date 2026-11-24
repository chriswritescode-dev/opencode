use std::pin::Pin;
use std::sync::LazyLock;

use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use futures_util::Stream;
use regex_lite::Regex;
use serde_json::{Map, Number, Value};

use crate::media::protocol::{Body, DecodeContext, Identity};
use crate::media::route::{Mode, ResponseContext, StreamedProtocol};
use crate::modalities::image::{MediaUsage, token_usage};
use crate::modalities::speech::{SpeechEvent, SpeechRequest};
use crate::protocols::shared::number_from_f64;
use crate::protocols::utils::gemini_generate_content::content_filtered;
use crate::protocols::utils::speech_stream as speech;
use crate::protocols::utils::speech_stream::PcmEncoding;
use crate::route::executor::ByteStream;
use crate::schema::{AiError, AiErrorReason, ProviderId, ProviderMetadata};

pub const ID: &str = "google-speech";
pub const NAME: &str = "Google Speech";
pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

const PROVIDER: &str = "google";
const DEFAULT_SAMPLE_RATE: i64 = 24000;

static GEMINI_38_TTS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^gemini-3\.8-.*-tts(?:-|$)").expect("gemini 3.8 tts pattern compiles"));

pub struct GoogleSpeech {
    provider: ProviderId,
}

pub fn protocol() -> GoogleSpeech {
    GoogleSpeech { provider: ProviderId::from(PROVIDER) }
}

#[derive(Default)]
pub struct State {
    audio: speech::Audio,
    mime_type: Option<String>,
    usage: Option<Map<String, Value>>,
    finish_reason: Option<String>,
    model_version: Option<String>,
    response_id: Option<String>,
}

pub fn path(request: &SpeechRequest) -> String {
    if request.mode == Mode::Stream {
        format!("/models/{}:streamGenerateContent?alt=sse", request.model.id)
    } else {
        format!("/models/{}:generateContent", request.model.id)
    }
}

impl StreamedProtocol<SpeechRequest, SpeechEvent, speech::Frame, State> for GoogleSpeech {
    fn id(&self) -> &str {
        ID
    }

    fn provider(&self) -> &ProviderId {
        &self.provider
    }

    fn unsupported(&self) -> &[&'static str] {
        &["instructions", "speed"]
    }

    fn body_from(&self, request: &SpeechRequest, _mode: Mode) -> Result<Body, AiError> {
        let identity = identity();
        if request.timestamps == Some(true) {
            return Err(identity.unsupported("media.timestamps", format!("{NAME} does not return timestamps")));
        }
        if request.format.as_deref() == Some("pcm")
            && request.mode == Mode::Generate
            && GEMINI_38_TTS.is_match(&request.model.id)
        {
            return Err(identity.unsupported(
                "media.format",
                format!(
                    "{NAME} returns WAV by default for Gemini 3.8 TTS unary requests; omit the format to accept it"
                ),
            ));
        }
        if let Some(format) = request.format.as_deref()
            && format != "pcm"
        {
            return Err(identity.unsupported(
                "media.format",
                format!(
                    "{NAME} only accepts raw PCM as an explicit format; omit it to accept the provider's default output"
                ),
            ));
        }
        let mut speech_config = Map::new();
        if let Some(voice_name) = speech::voice_id(request.voice.as_ref()) {
            let mut prebuilt = Map::new();
            prebuilt.insert("voiceName".to_string(), Value::String(voice_name));
            let mut voice_config = Map::new();
            voice_config.insert("prebuiltVoiceConfig".to_string(), Value::Object(prebuilt));
            speech_config.insert("voiceConfig".to_string(), Value::Object(voice_config));
        }
        if let Some(language) = &request.language {
            speech_config.insert("languageCode".to_string(), Value::String(language.clone()));
        }
        let mut config = Map::new();
        config.insert("responseModalities".to_string(), Value::Array(vec![Value::String("AUDIO".to_string())]));
        config.insert("speechConfig".to_string(), Value::Object(speech_config));
        let options = request.provider_options.clone().unwrap_or_default();
        let generation_config = crate::json::merge_records([&config, &options]).unwrap_or_default();
        let mut body = Map::new();
        let contents = vec![serde_json::json!({ "role": "user", "parts": [{ "text": request.text }] })];
        body.insert("contents".to_string(), Value::Array(contents));
        body.insert("generationConfig".to_string(), Value::Object(generation_config));
        let http = http_body(request);
        let value = crate::json::merge_records([&body, &http]).unwrap_or_default();
        Ok(Body::Json { value, query: None })
    }

    fn frames(
        &self,
        bytes: ByteStream,
        context: &DecodeContext<SpeechRequest>,
    ) -> Pin<Box<dyn Stream<Item = Result<speech::Frame, AiError>> + Send>> {
        if context.mode == Mode::Stream { speech::sse(bytes) } else { speech::document(bytes) }
    }

    fn initial(&self) -> State {
        State::default()
    }

    fn step(&self, state: State, frame: speech::Frame) -> Result<(State, Vec<SpeechEvent>), AiError> {
        match frame {
            speech::Frame::Text(frame) => on_chunk(state, &frame),
            speech::Frame::Bytes(_) => Ok((state, Vec::new())),
        }
    }

    fn finish(&self, state: &State, context: &ResponseContext<SpeechRequest>) -> Result<Vec<SpeechEvent>, AiError> {
        let identity = identity();
        let Some(finish_reason) = state.finish_reason.as_deref() else {
            return Err(identity.incomplete());
        };
        let sample_rate =
            speech::sample_rate(state.mime_type.as_deref()).unwrap_or_else(|| Number::from(DEFAULT_SAMPLE_RATE));
        let wav = state
            .mime_type
            .as_deref()
            .is_some_and(|mime| mime.split(';').next().is_some_and(|prefix| prefix.eq_ignore_ascii_case("audio/wav")));
        let mut output = if wav {
            speech::container("wav", Some(sample_rate))
        } else {
            let media_type =
                state.mime_type.clone().unwrap_or_else(|| format!("audio/L16;codec=pcm;rate={sample_rate}"));
            speech::pcm(PcmEncoding::S16le, Some(sample_rate), Some(media_type))
        };
        let format = output.info.as_ref().and_then(|info| info.format.as_deref()).unwrap_or_default();
        if context.request.format.as_deref() == Some("pcm") && format != "pcm" {
            return Err(
                identity.frame_error(format!("{NAME} returned {format} instead of the requested raw PCM"), None)
            );
        }
        output.usage = usage(state.usage.as_ref());
        output.provider_metadata = Some(provider_metadata(state));
        output.detail = Some(format!("finish reason: {finish_reason}"));
        speech::finish(&identity, &state.audio, output)
    }
}

fn on_chunk(state: State, frame: &str) -> Result<(State, Vec<SpeechEvent>), AiError> {
    let identity = identity();
    let invalid = || identity.frame_error(format!("{NAME} sent an invalid stream event"), Some(frame.to_string()));
    let value: Value = serde_json::from_str(frame).map_err(|_| invalid())?;
    let object = value.as_object().ok_or_else(invalid)?;
    if let Some(feedback) = object.get("promptFeedback").and_then(Value::as_object)
        && let Some(reason) = feedback.get("blockReason").and_then(Value::as_str)
    {
        let message = match feedback.get("blockReasonMessage").and_then(Value::as_str) {
            Some(message) => format!("{NAME} blocked the request ({reason}): {message}"),
            None => format!("{NAME} blocked the request ({reason})"),
        };
        return Err(AiError {
            reason: AiErrorReason::ContentPolicy { message, body: Some(frame.to_string()), http: None },
        });
    }
    let chunk_finish_reason = object
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|candidates| candidates.first())
        .and_then(|candidate| candidate.get("finishReason"))
        .and_then(Value::as_str);
    let mut state = track(state, object);
    let parts = object
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|candidates| candidates.first())
        .and_then(|candidate| candidate.get("content"))
        .and_then(|content| content.get("parts"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut chunks = Vec::new();
    for part in &parts {
        let Some(inline) = part.get("inlineData") else { continue };
        let inline = inline.as_object().ok_or_else(invalid)?;
        let mime_type = inline.get("mimeType").and_then(Value::as_str).ok_or_else(invalid)?;
        let data = inline.get("data").and_then(Value::as_str).ok_or_else(invalid)?;
        let bytes = BASE64_STANDARD.decode(data).map_err(|_| invalid())?;
        if state.mime_type.is_none() {
            state.mime_type = Some(mime_type.to_string());
        }
        chunks.push(bytes);
    }
    let mut events = Vec::new();
    for chunk in chunks {
        events.extend(speech::delta(&mut state.audio, &chunk));
    }
    if state.audio.chunks.is_empty() && content_filtered(chunk_finish_reason) {
        return Err(AiError {
            reason: AiErrorReason::ContentPolicy {
                message: format!("{NAME} withheld its output ({})", chunk_finish_reason.unwrap_or_default()),
                body: Some(frame.to_string()),
                http: None,
            },
        });
    }
    Ok((state, events))
}

fn track(mut state: State, object: &Map<String, Value>) -> State {
    if let Some(usage) = object.get("usageMetadata").and_then(Value::as_object) {
        state.usage = Some(
            ["promptTokenCount", "candidatesTokenCount", "totalTokenCount"]
                .into_iter()
                .filter_map(|key| {
                    usage
                        .get(key)
                        .and_then(Value::as_f64)
                        .and_then(number_from_f64)
                        .map(|value| (key.to_string(), Value::Number(value)))
                })
                .collect(),
        );
    }
    if let Some(reason) = object
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|candidates| candidates.first())
        .and_then(|candidate| candidate.get("finishReason"))
        .and_then(Value::as_str)
    {
        state.finish_reason = Some(reason.to_string());
    }
    if let Some(version) = object.get("modelVersion").and_then(Value::as_str) {
        state.model_version = Some(version.to_string());
    }
    if let Some(id) = object.get("responseId").and_then(Value::as_str) {
        state.response_id = Some(id.to_string());
    }
    state
}

fn usage(state: Option<&Map<String, Value>>) -> Option<MediaUsage> {
    let state = state?;
    let input = state.get("promptTokenCount").and_then(Value::as_number).cloned();
    let output = state.get("candidatesTokenCount").and_then(Value::as_number).cloned();
    let total = state.get("totalTokenCount").and_then(Value::as_number).cloned().or_else(|| {
        if input.is_none() && output.is_none() {
            return None;
        }
        number_from_f64(
            input.as_ref().and_then(Number::as_f64).unwrap_or_default()
                + output.as_ref().and_then(Number::as_f64).unwrap_or_default(),
        )
    });
    let mut details = Map::new();
    details.insert("google".to_string(), Value::Object(state.clone()));
    token_usage(input, output, total, Some(details))
}

fn provider_metadata(state: &State) -> ProviderMetadata {
    let mut google = Map::new();
    if let Some(finish_reason) = &state.finish_reason {
        google.insert("finishReason".to_string(), Value::String(finish_reason.clone()));
    }
    if let Some(model_version) = &state.model_version {
        google.insert("modelVersion".to_string(), Value::String(model_version.clone()));
    }
    if let Some(response_id) = &state.response_id {
        google.insert("responseId".to_string(), Value::String(response_id.clone()));
    }
    let mut metadata = Map::new();
    metadata.insert("google".to_string(), Value::Object(google));
    metadata
}

fn identity() -> Identity {
    Identity { id: ID.to_string(), name: NAME.to_string(), provider: ProviderId::from(PROVIDER) }
}

fn http_body(request: &SpeechRequest) -> Map<String, Value> {
    request.http.as_ref().and_then(|http| http.body.clone()).unwrap_or_default()
}
