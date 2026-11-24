use std::pin::Pin;
use std::sync::LazyLock;

use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use futures_util::Stream;
use regex_lite::Regex;
use serde_json::{Map, Number, Value};

use crate::media::protocol::{Body, DecodeContext, Identity};
use crate::media::route::{Mode, ResponseContext, StreamedProtocol};
use crate::modalities::speech::{SpeechEvent, SpeechRequest};
use crate::protocols::utils::gemini_generate_content;
use crate::protocols::utils::media_input::merge_body;
use crate::protocols::utils::speech_stream as speech;
use crate::protocols::utils::speech_stream::PcmEncoding;
use crate::route::executor::ByteStream;
use crate::schema::{AiError, ProviderId};

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
    metadata: gemini_generate_content::Metadata,
}

pub fn path(request: &SpeechRequest) -> String {
    gemini_generate_content::path(&request.model.id, request.mode)
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
        let generation_config = merge_body(config, request.provider_options.as_ref(), None);
        let mut body = Map::new();
        let contents = vec![serde_json::json!({ "role": "user", "parts": [{ "text": request.text }] })];
        body.insert("contents".to_string(), Value::Array(contents));
        body.insert("generationConfig".to_string(), Value::Object(generation_config));
        let value = merge_body(body, None, request.http.as_ref());
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
        let Some(finish_reason) = state.metadata.finish_reason() else {
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
        output.usage = state.metadata.usage();
        output.provider_metadata = Some(state.metadata.provider_metadata());
        output.detail = Some(format!("finish reason: {finish_reason}"));
        speech::finish(&identity, &state.audio, output)
    }
}

fn on_chunk(mut state: State, frame: &str) -> Result<(State, Vec<SpeechEvent>), AiError> {
    let identity = identity();
    let invalid = || identity.frame_error(format!("{NAME} sent an invalid stream event"), Some(frame.to_string()));
    let value: Value = serde_json::from_str(frame).map_err(|_| invalid())?;
    let object = value.as_object().ok_or_else(invalid)?;
    if let Some(blocked) = gemini_generate_content::blocked(NAME, object, frame) {
        return Err(blocked);
    }
    let chunk_finish_reason = object
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|candidates| candidates.first())
        .and_then(|candidate| candidate.get("finishReason"))
        .and_then(Value::as_str);
    state.metadata.track(object);
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
    if state.audio.chunks.is_empty()
        && let Some(withheld) = gemini_generate_content::withheld(NAME, chunk_finish_reason, frame)
    {
        return Err(withheld);
    }
    Ok((state, events))
}

fn identity() -> Identity {
    Identity { id: ID.to_string(), name: NAME.to_string(), provider: ProviderId::from(PROVIDER) }
}
