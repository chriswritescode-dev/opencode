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
use crate::modalities::speech::{SpeechEvent, SpeechRequest, SpeechVoice};
use crate::protocols::utils::media_input::merge_body;
use crate::protocols::utils::speech_stream as speech;
use crate::protocols::utils::speech_stream::PcmEncoding;
use crate::route::executor::ByteStream;
use crate::schema::{AiError, ProviderId};

pub const ID: &str = "openai-speech";
pub const NAME: &str = "OpenAI Speech";
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
pub const PATH: &str = "/audio/speech";

const PROVIDER: &str = "openai";
const PCM_SAMPLE_RATE: i64 = 24000;
const FORMATS: [&str; 6] = ["mp3", "opus", "aac", "flac", "wav", "pcm"];

static TTS1: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^tts-1(-hd)?(-|$)").expect("tts-1 pattern compiles"));

pub struct OpenAiSpeech {
    provider: ProviderId,
}

pub fn protocol() -> OpenAiSpeech {
    OpenAiSpeech { provider: ProviderId::from(PROVIDER) }
}

#[derive(Default)]
pub struct State {
    audio: speech::Audio,
    done: bool,
    usage: Option<MediaUsage>,
}

impl speech::AudioState for State {
    fn audio_mut(&mut self) -> &mut speech::Audio {
        &mut self.audio
    }
}

impl StreamedProtocol<SpeechRequest, SpeechEvent, speech::Frame, State> for OpenAiSpeech {
    fn id(&self) -> &str {
        ID
    }

    fn provider(&self) -> &ProviderId {
        &self.provider
    }

    fn unsupported(&self) -> &[&'static str] {
        &["language"]
    }

    fn body_from(&self, request: &SpeechRequest, _mode: Mode) -> Result<Body, AiError> {
        let identity = identity();
        if request.timestamps == Some(true) {
            return Err(identity.unsupported("media.timestamps", format!("{NAME} does not return timestamps")));
        }
        if let Some(format) = request.format.as_deref()
            && !FORMATS.contains(&format)
        {
            return Err(identity.unsupported(
                "media.format",
                format!("{NAME} supports the mp3, opus, aac, flac, wav, and pcm formats, not \"{format}\""),
            ));
        }
        let mut base = Map::new();
        base.insert("model".to_string(), Value::String(request.model.id.clone()));
        base.insert("input".to_string(), Value::String(request.text.clone()));
        insert(&mut base, "voice", voice(request.voice.as_ref()));
        insert(&mut base, "instructions", request.instructions.as_ref().map(|value| Value::String(value.clone())));
        insert(&mut base, "response_format", request.format.as_ref().map(|value| Value::String(value.clone())));
        insert(&mut base, "speed", request.speed.as_ref().map(|value| Value::Number(value.clone())));
        if request.mode == Mode::Stream && supports_sse(&request.model.id) {
            base.insert("stream_format".to_string(), Value::String("sse".to_string()));
        }
        let value = merge_body(base, request.provider_options.as_ref(), request.http.as_ref());
        Ok(Body::Json { value, query: None })
    }

    fn frames(
        &self,
        bytes: ByteStream,
        context: &DecodeContext<SpeechRequest>,
    ) -> Pin<Box<dyn Stream<Item = Result<speech::Frame, AiError>> + Send>> {
        if is_sse(&context.request) { speech::sse(bytes) } else { speech::bytes(bytes) }
    }

    fn initial(&self) -> State {
        State::default()
    }

    fn step(&self, state: State, frame: speech::Frame) -> Result<(State, Vec<SpeechEvent>), AiError> {
        speech::step(state, frame, |state, frame| self.on_event(state, frame))
    }

    fn finish(&self, state: &State, context: &ResponseContext<SpeechRequest>) -> Result<Vec<SpeechEvent>, AiError> {
        let identity = identity();
        if is_sse(&context.request) && !state.done {
            return Err(identity.incomplete());
        }
        let format = sent_format(&context.request);
        let mut output = if format == "pcm" {
            speech::pcm(PcmEncoding::S16le, Some(Number::from(PCM_SAMPLE_RATE)), None)
        } else {
            speech::container(&format, None)
        };
        output.usage = state.usage.clone();
        speech::finish(&identity, &state.audio, output)
    }
}

impl OpenAiSpeech {
    fn on_event(&self, state: State, frame: &str) -> Result<(State, Vec<SpeechEvent>), AiError> {
        let identity = identity();
        let invalid = || identity.frame_error(format!("{NAME} sent an invalid stream event"), Some(frame.to_string()));
        let value: Value = serde_json::from_str(frame).map_err(|_| invalid())?;
        let object = value.as_object().ok_or_else(invalid)?;
        let kind = object.get("type").and_then(Value::as_str).ok_or_else(invalid)?;
        match kind {
            "speech.audio.delta" => {
                let data = object.get("audio").and_then(Value::as_str).ok_or_else(invalid)?;
                let chunk = BASE64_STANDARD.decode(data).map_err(|_| invalid())?;
                let mut state = state;
                let events = speech::delta(&mut state.audio, &chunk);
                Ok((state, events))
            }
            "speech.audio.done" => {
                let usage = done_usage(object.get("usage"), frame)?;
                Ok((State { audio: state.audio, done: true, usage }, Vec::new()))
            }
            _ => Ok((state, Vec::new())),
        }
    }
}

fn identity() -> Identity {
    Identity { id: ID.to_string(), name: NAME.to_string(), provider: ProviderId::from(PROVIDER) }
}

fn voice(voice: Option<&SpeechVoice>) -> Option<Value> {
    voice.map(|voice| match voice {
        SpeechVoice::Name(name) => Value::String(name.clone()),
        SpeechVoice::Id { id } => {
            let mut object = Map::new();
            object.insert("id".to_string(), Value::String(id.clone()));
            Value::Object(object)
        }
    })
}

fn supports_sse(model: &str) -> bool {
    !TTS1.is_match(model)
}

fn is_sse(request: &SpeechRequest) -> bool {
    match stream_format(request) {
        Some(value) => value.as_str() == Some("sse"),
        None => request.mode == Mode::Stream && supports_sse(&request.model.id),
    }
}

fn stream_format(request: &SpeechRequest) -> Option<&Value> {
    request
        .http
        .as_ref()
        .and_then(|http| http.body.as_ref())
        .and_then(|body| body.get("stream_format"))
        .or_else(|| request.provider_options.as_ref().and_then(|options| options.get("stream_format")))
}

fn sent_format(request: &SpeechRequest) -> String {
    if let Some(value) =
        request.http.as_ref().and_then(|http| http.body.as_ref()).and_then(|body| body.get("response_format"))
    {
        return value.as_str().map(str::to_string).unwrap_or_else(|| "mp3".to_string());
    }
    if let Some(value) = request.provider_options.as_ref().and_then(|options| options.get("response_format")) {
        return value.as_str().map(str::to_string).unwrap_or_else(|| "mp3".to_string());
    }
    request.format.clone().unwrap_or_else(|| "mp3".to_string())
}

fn done_usage(value: Option<&Value>, frame: &str) -> Result<Option<MediaUsage>, AiError> {
    let Some(value) = value else { return Ok(None) };
    let identity = identity();
    let invalid = || identity.frame_error(format!("{NAME} sent an invalid stream event"), Some(frame.to_string()));
    let usage = value.as_object().ok_or_else(invalid)?;
    let mut details = Map::new();
    let mut present = Map::new();
    for key in ["input_tokens", "output_tokens", "total_tokens"] {
        if let Some(value) = usage.get(key) {
            present.insert(key.to_string(), value.clone());
        }
    }
    details.insert("openai".to_string(), Value::Object(present));
    let input = token_number(usage, "input_tokens", &invalid)?;
    let output = token_number(usage, "output_tokens", &invalid)?;
    let total = token_number(usage, "total_tokens", &invalid)?;
    Ok(token_usage(input, output, total, Some(details)))
}

fn token_number(
    usage: &Map<String, Value>,
    key: &str,
    invalid: &impl Fn() -> AiError,
) -> Result<Option<Number>, AiError> {
    match usage.get(key) {
        None => Ok(None),
        Some(value) => value.as_f64().and_then(crate::protocols::shared::number_from_f64).map(Some).ok_or_else(invalid),
    }
}

fn insert(target: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        target.insert(key.to_string(), value);
    }
}
