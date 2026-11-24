use std::pin::Pin;

use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use futures_util::Stream;
use serde_json::{Map, Number, Value};

use crate::media::protocol::{Body, DecodeContext, Identity};
use crate::media::route::{Mode, ResponseContext, StreamedProtocol};
use crate::modalities::speech::{SpeechEvent, SpeechRequest};
use crate::protocols::shared;
use crate::protocols::shared::number_from_f64;
use crate::protocols::utils::media_input::merge_body;
use crate::protocols::utils::speech_stream as speech;
use crate::protocols::utils::speech_stream::PcmEncoding;
use crate::provider_error::{ProviderFailure, classify_provider_failure};
use crate::route::executor::ByteStream;
use crate::schema::{AiError, ProviderId};

pub const ID: &str = "cartesia-speech";
pub const NAME: &str = "Cartesia";
pub const DEFAULT_BASE_URL: &str = "https://api.cartesia.ai";
pub const API_VERSION: &str = "2026-08-14";
pub const BYTES_PATH: &str = "/tts/bytes";
pub const SSE_PATH: &str = "/tts/sse";

const PROVIDER: &str = "cartesia";
const DEFAULT_SAMPLE_RATE: i64 = 44100;
const DEFAULT_BIT_RATE: i64 = 128000;

pub struct CartesiaSpeech {
    provider: ProviderId,
}

pub fn protocol() -> CartesiaSpeech {
    CartesiaSpeech { provider: ProviderId::from(PROVIDER) }
}

#[derive(Default)]
pub struct State {
    audio: speech::Audio,
    done: bool,
}

pub fn uses_sse(request: &SpeechRequest) -> bool {
    request.mode == Mode::Stream || request.timestamps == Some(true)
}

pub fn path(request: &SpeechRequest) -> String {
    if uses_sse(request) { SSE_PATH.to_string() } else { BYTES_PATH.to_string() }
}

impl speech::AudioState for State {
    fn audio_mut(&mut self) -> &mut speech::Audio {
        &mut self.audio
    }
}

impl StreamedProtocol<SpeechRequest, SpeechEvent, speech::Frame, State> for CartesiaSpeech {
    fn id(&self) -> &str {
        ID
    }

    fn provider(&self) -> &ProviderId {
        &self.provider
    }

    fn unsupported(&self) -> &[&'static str] {
        &["instructions"]
    }

    fn body_from(&self, request: &SpeechRequest, _mode: Mode) -> Result<Body, AiError> {
        let voice = speech::voice_id(request.voice.as_ref())
            .ok_or_else(|| shared::invalid_request(format!("{NAME} requires a voice id; pass it as `voice`")))?;
        let format = output_format(request)?;
        let mut output_format = Map::new();
        output_format.insert("container".to_string(), Value::String(format.container.clone()));
        if format.container == "mp3" {
            output_format.insert("sample_rate".to_string(), Value::Number(format.sample_rate.clone()));
            if let Some(bit_rate) = format.bit_rate {
                output_format.insert("bit_rate".to_string(), Value::Number(bit_rate));
            }
        } else {
            if let Some(encoding) = &format.encoding {
                output_format.insert("encoding".to_string(), Value::String(encoding.clone()));
            }
            output_format.insert("sample_rate".to_string(), Value::Number(format.sample_rate.clone()));
        }
        let mut base = Map::new();
        base.insert("model_id".to_string(), Value::String(request.model.id.clone()));
        base.insert("transcript".to_string(), Value::String(request.text.clone()));
        base.insert("voice".to_string(), Value::String(voice));
        base.insert("output_format".to_string(), Value::Object(output_format));
        insert(&mut base, "language", request.language.as_ref().map(|value| Value::String(value.clone())));
        insert(
            &mut base,
            "generation_config",
            request.speed.as_ref().map(|speed| {
                let mut config = Map::new();
                config.insert("speed".to_string(), Value::Number(speed.clone()));
                Value::Object(config)
            }),
        );
        if request.timestamps == Some(true) {
            base.insert("add_timestamps".to_string(), Value::Bool(true));
        }
        let native = native_options(request.provider_options.as_ref());
        let value = merge_body(base, Some(&native), request.http.as_ref());
        Ok(Body::Json { value, query: None })
    }

    fn frames(
        &self,
        bytes: ByteStream,
        context: &DecodeContext<SpeechRequest>,
    ) -> Pin<Box<dyn Stream<Item = Result<speech::Frame, AiError>> + Send>> {
        if uses_sse(&context.request) { speech::sse(bytes) } else { speech::bytes(bytes) }
    }

    fn initial(&self) -> State {
        State::default()
    }

    fn step(&self, state: State, frame: speech::Frame) -> Result<(State, Vec<SpeechEvent>), AiError> {
        speech::step(state, frame, on_event)
    }

    fn finish(&self, state: &State, context: &ResponseContext<SpeechRequest>) -> Result<Vec<SpeechEvent>, AiError> {
        let identity = identity();
        if uses_sse(&context.request) && !state.done {
            return Err(identity.incomplete());
        }
        let format = output_format(&context.request)?;
        let output = if format.container == "raw" {
            let encoding = format.encoding.as_deref().and_then(pcm_encoding).unwrap_or(PcmEncoding::S16le);
            speech::pcm(encoding, Some(format.sample_rate), None)
        } else {
            speech::container(&format.container, Some(format.sample_rate))
        };
        speech::finish(&identity, &state.audio, output)
    }
}

fn on_event(state: State, frame: &str) -> Result<(State, Vec<SpeechEvent>), AiError> {
    let identity = identity();
    let invalid = || identity.frame_error(format!("{NAME} sent an invalid stream event"), Some(frame.to_string()));
    let value: Value = serde_json::from_str(frame).map_err(|_| invalid())?;
    let object = value.as_object().ok_or_else(invalid)?;
    let kind = object.get("type").and_then(Value::as_str).ok_or_else(invalid)?;
    match kind {
        "chunk" => {
            let Some(data) = object.get("data") else { return Ok((state, Vec::new())) };
            let data = data.as_str().ok_or_else(invalid)?;
            let chunk = BASE64_STANDARD.decode(data).map_err(|_| invalid())?;
            let mut state = state;
            let events = speech::delta(&mut state.audio, &chunk);
            Ok((state, events))
        }
        "timestamps" => {
            let Some(word_timestamps) = object.get("word_timestamps") else {
                return Ok((state, Vec::new()));
            };
            let word_timestamps = word_timestamps.as_object().ok_or_else(invalid)?;
            let words = string_array(word_timestamps.get("words")).ok_or_else(invalid)?;
            let starts = number_array(word_timestamps.get("start")).ok_or_else(invalid)?;
            let ends = number_array(word_timestamps.get("end")).ok_or_else(invalid)?;
            Ok((state, speech::timestamps(&words, &starts, &ends)))
        }
        "done" => Ok((State { audio: state.audio, done: true }, Vec::new())),
        "error" => {
            let title = object.get("title").and_then(Value::as_str);
            let detail = object.get("message").and_then(Value::as_str).unwrap_or("unknown error");
            let message = format!(
                "{NAME} stream failed{}: {detail}",
                title.map(|title| format!(" ({title})")).unwrap_or_default()
            );
            let status = object.get("status_code").and_then(Value::as_u64).map(|status| status as u16);
            Err(AiError {
                reason: classify_provider_failure(ProviderFailure {
                    message,
                    status,
                    raw_body: Some(frame.to_string()),
                    ..Default::default()
                }),
            })
        }
        _ => Ok((state, Vec::new())),
    }
}

struct OutputFormat {
    container: String,
    sample_rate: Number,
    bit_rate: Option<Number>,
    encoding: Option<String>,
}

fn output_format(request: &SpeechRequest) -> Result<OutputFormat, AiError> {
    let sse = uses_sse(request);
    let format = request.format.clone().unwrap_or_else(|| if sse { "pcm".to_string() } else { "mp3".to_string() });
    let container = match format.as_str() {
        "pcm" => "raw",
        "wav" => "wav",
        "mp3" => "mp3",
        _ => {
            return Err(identity().unsupported(
                "media.format",
                format!("{NAME} supports the pcm, wav, and mp3 formats, not \"{format}\""),
            ));
        }
    };
    if sse && container != "raw" {
        return Err(identity().unsupported(
            "media.format",
            format!("{NAME} streams and timestamps only raw PCM; request format \"pcm\" instead of \"{format}\""),
        ));
    }
    let sample_rate = number_option(request.provider_options.as_ref(), "sampleRate")
        .unwrap_or_else(|| Number::from(DEFAULT_SAMPLE_RATE));
    if container == "mp3" {
        return Ok(OutputFormat {
            container: container.to_string(),
            sample_rate,
            bit_rate: Some(
                number_option(request.provider_options.as_ref(), "bitRate")
                    .unwrap_or_else(|| Number::from(DEFAULT_BIT_RATE)),
            ),
            encoding: None,
        });
    }
    Ok(OutputFormat {
        container: container.to_string(),
        sample_rate,
        bit_rate: None,
        encoding: Some(
            string_option(request.provider_options.as_ref(), "encoding").unwrap_or_else(|| "pcm_s16le".to_string()),
        ),
    })
}

fn pcm_encoding(value: &str) -> Option<PcmEncoding> {
    match value {
        "pcm_s16le" => Some(PcmEncoding::S16le),
        "pcm_f32le" => Some(PcmEncoding::F32le),
        "pcm_mulaw" => Some(PcmEncoding::Mulaw),
        "pcm_alaw" => Some(PcmEncoding::Alaw),
        _ => None,
    }
}

fn native_options(options: Option<&Map<String, Value>>) -> Map<String, Value> {
    let Some(options) = options else { return Map::new() };
    options
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), "sampleRate" | "bitRate" | "encoding"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn number_option(options: Option<&Map<String, Value>>, key: &str) -> Option<Number> {
    options.and_then(|options| options.get(key)).and_then(Value::as_number).cloned()
}

fn string_option(options: Option<&Map<String, Value>>, key: &str) -> Option<String> {
    options.and_then(|options| options.get(key)).and_then(Value::as_str).map(str::to_string)
}

fn string_array(value: Option<&Value>) -> Option<Vec<String>> {
    value?.as_array()?.iter().map(|item| item.as_str().map(str::to_string)).collect()
}

fn number_array(value: Option<&Value>) -> Option<Vec<Number>> {
    value?.as_array()?.iter().map(|item| item.as_f64().and_then(number_from_f64)).collect()
}

fn identity() -> Identity {
    Identity { id: ID.to_string(), name: NAME.to_string(), provider: ProviderId::from(PROVIDER) }
}

fn insert(target: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        target.insert(key.to_string(), value);
    }
}
