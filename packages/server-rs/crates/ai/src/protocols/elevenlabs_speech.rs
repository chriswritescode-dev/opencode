use std::pin::Pin;

use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use futures_util::Stream;
use serde_json::{Map, Number, Value};

use crate::media::protocol::{Body, DecodeContext, Identity, QueryValue};
use crate::media::route::{Mode, ResponseContext, StreamedProtocol};
use crate::modalities::speech::{SpeechEvent, SpeechRequest};
use crate::protocols::shared;
use crate::protocols::utils::speech_stream as speech;
use crate::protocols::utils::speech_stream::{HeaderUsageKind, PcmEncoding};
use crate::route::executor::ByteStream;
use crate::schema::{AiError, ProviderId};

pub const ID: &str = "elevenlabs-speech";
pub const NAME: &str = "ElevenLabs";
pub const DEFAULT_BASE_URL: &str = "https://api.elevenlabs.io";
pub const PATH: &str = "/v1/text-to-speech";

const PROVIDER: &str = "elevenlabs";

pub struct ElevenLabsSpeech {
    provider: ProviderId,
}

pub fn protocol() -> ElevenLabsSpeech {
    ElevenLabsSpeech { provider: ProviderId::from(PROVIDER) }
}

#[derive(Default)]
pub struct State {
    audio: speech::Audio,
}

pub fn path(request: &SpeechRequest) -> String {
    let voice = speech::voice_id(request.voice.as_ref()).unwrap_or_default();
    let stream = if request.mode == Mode::Stream { "/stream" } else { "" };
    let timestamps = if request.timestamps == Some(true) { "/with-timestamps" } else { "" };
    format!("{PATH}/{}{stream}{timestamps}", opencode_api_types::js::encode_component(&voice))
}

impl StreamedProtocol<SpeechRequest, SpeechEvent, speech::Frame, State> for ElevenLabsSpeech {
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
        if request.voice.is_none() {
            return Err(shared::invalid_request(format!("{NAME} requires a voice id; pass it as `voice`")));
        }
        let mut base = Map::new();
        base.insert("text".to_string(), Value::String(request.text.clone()));
        base.insert("model_id".to_string(), Value::String(request.model.id.clone()));
        insert(&mut base, "language_code", request.language.as_ref().map(|value| Value::String(value.clone())));
        insert(
            &mut base,
            "voice_settings",
            request.speed.as_ref().map(|speed| {
                let mut settings = Map::new();
                settings.insert("speed".to_string(), Value::Number(speed.clone()));
                Value::Object(settings)
            }),
        );
        let native = native_options(request.provider_options.as_ref());
        let http = http_body(request);
        let value = crate::json::merge_records([&base, &native, &http]).unwrap_or_default();
        let query = vec![("output_format".to_string(), QueryValue::One(output_format(request)?))];
        Ok(Body::Json { value, query: Some(query) })
    }

    fn frames(
        &self,
        bytes: ByteStream,
        context: &DecodeContext<SpeechRequest>,
    ) -> Pin<Box<dyn Stream<Item = Result<speech::Frame, AiError>> + Send>> {
        if context.request.timestamps != Some(true) {
            return speech::bytes(bytes);
        }
        if context.request.mode == Mode::Stream { speech::lines(bytes) } else { speech::document(bytes) }
    }

    fn initial(&self) -> State {
        State::default()
    }

    fn step(&self, state: State, frame: speech::Frame) -> Result<(State, Vec<SpeechEvent>), AiError> {
        match frame {
            speech::Frame::Text(frame) => on_record(state, &frame),
            speech::Frame::Bytes(bytes) => {
                let mut state = state;
                let events = speech::delta(&mut state.audio, &bytes);
                Ok((state, events))
            }
        }
    }

    fn finish(&self, state: &State, context: &ResponseContext<SpeechRequest>) -> Result<Vec<SpeechEvent>, AiError> {
        let identity = identity();
        let mut output = describe_output(&output_format(&context.request)?);
        output.usage = speech::header_usage(
            HeaderUsageKind::Credits,
            context.http.headers.get("character-cost").map(String::as_str),
        );
        output.provider_metadata = context.http.headers.get("request-id").map(|request_id| {
            let mut elevenlabs = Map::new();
            elevenlabs.insert("requestId".to_string(), Value::String(request_id.clone()));
            let mut metadata = Map::new();
            metadata.insert("elevenlabs".to_string(), Value::Object(elevenlabs));
            metadata
        });
        speech::finish(&identity, &state.audio, output)
    }
}

fn on_record(state: State, frame: &str) -> Result<(State, Vec<SpeechEvent>), AiError> {
    let identity = identity();
    let invalid = || identity.frame_error(format!("{NAME} sent an invalid stream event"), Some(frame.to_string()));
    let value: Value = serde_json::from_str(frame).map_err(|_| invalid())?;
    let object = value.as_object().ok_or_else(invalid)?;
    let audio = object.get("audio_base64").and_then(Value::as_str).ok_or_else(invalid)?;
    let chunk = BASE64_STANDARD.decode(audio).map_err(|_| invalid())?;
    let mut state = state;
    let mut events = speech::delta(&mut state.audio, &chunk);
    match object.get("alignment") {
        None | Some(Value::Null) => {}
        Some(Value::Object(alignment)) => {
            let characters = string_array(alignment.get("characters")).ok_or_else(invalid)?;
            let starts = number_array(alignment.get("character_start_times_seconds")).ok_or_else(invalid)?;
            let ends = number_array(alignment.get("character_end_times_seconds")).ok_or_else(invalid)?;
            events.extend(speech::timestamps(&characters, &starts, &ends));
        }
        Some(_) => return Err(invalid()),
    }
    Ok((state, events))
}

fn output_format(request: &SpeechRequest) -> Result<String, AiError> {
    let format = request
        .provider_options
        .as_ref()
        .and_then(|options| options.get("outputFormat"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| default_output_format(request.format.as_deref().unwrap_or("mp3")).map(str::to_string));
    let Some(format) = format else {
        return Err(identity().unsupported(
            "media.format",
            format!(
                "{NAME} has no default output format for \"{}\"; pass providerOptions.outputFormat",
                request.format.as_deref().unwrap_or("undefined")
            ),
        ));
    };
    if request.mode == Mode::Stream && format.starts_with("wav_") {
        return Err(identity().unsupported(
            "media.format",
            format!("{NAME} streams mp3, pcm, opus, ulaw, and alaw but not \"{format}\"; use generate for WAV"),
        ));
    }
    Ok(format)
}

fn default_output_format(format: &str) -> Option<&'static str> {
    match format {
        "mp3" => Some("mp3_44100_128"),
        "pcm" => Some("pcm_24000"),
        "wav" => Some("wav_24000"),
        "opus" => Some("opus_48000_64"),
        _ => None,
    }
}

fn describe_output(format: &str) -> speech::SpeechOutput {
    let mut parts = format.split('_');
    let codec = parts.next().unwrap_or(format);
    let rate = parts.next().and_then(|rate| rate.parse::<f64>().ok()).and_then(shared::number_from_f64);
    match codec {
        "pcm" => speech::pcm(PcmEncoding::S16le, rate, None),
        "ulaw" => speech::pcm(PcmEncoding::Mulaw, rate, None),
        "alaw" => speech::pcm(PcmEncoding::Alaw, rate, None),
        _ => speech::container(codec, rate),
    }
}

fn native_options(options: Option<&Map<String, Value>>) -> Map<String, Value> {
    let Some(options) = options else { return Map::new() };
    options
        .iter()
        .filter(|(key, _)| key.as_str() != "outputFormat")
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn string_array(value: Option<&Value>) -> Option<Vec<String>> {
    value?.as_array()?.iter().map(|item| item.as_str().map(str::to_string)).collect()
}

fn number_array(value: Option<&Value>) -> Option<Vec<Number>> {
    value?.as_array()?.iter().map(|item| item.as_f64().and_then(shared::number_from_f64)).collect()
}

fn identity() -> Identity {
    Identity { id: ID.to_string(), name: NAME.to_string(), provider: ProviderId::from(PROVIDER) }
}

fn http_body(request: &SpeechRequest) -> Map<String, Value> {
    request.http.as_ref().and_then(|http| http.body.clone()).unwrap_or_default()
}

fn insert(target: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        target.insert(key.to_string(), value);
    }
}
