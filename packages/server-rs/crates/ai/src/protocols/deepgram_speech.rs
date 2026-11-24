use std::pin::Pin;

use futures_util::Stream;
use serde_json::{Map, Number, Value};

use crate::media::protocol::{Body, DecodeContext, Identity, Query, QueryValue};
use crate::media::route::{Mode, ResponseContext, StreamedProtocol};
use crate::modalities::speech::{SpeechEvent, SpeechRequest};
use crate::protocols::shared;
use crate::protocols::utils::speech_stream as speech;
use crate::protocols::utils::speech_stream::{HeaderUsageKind, PcmEncoding};
use crate::route::executor::ByteStream;
use crate::schema::{AiError, MediaInfo, ProviderId, ProviderMetadata};

pub const ID: &str = "deepgram-speech";
pub const NAME: &str = "Deepgram";
pub const DEFAULT_BASE_URL: &str = "https://api.deepgram.com";
pub const PATH: &str = "/v1/speak";

const PROVIDER: &str = "deepgram";

pub struct DeepgramSpeech {
    provider: ProviderId,
}

pub fn protocol() -> DeepgramSpeech {
    DeepgramSpeech { provider: ProviderId::from(PROVIDER) }
}

#[derive(Default)]
pub struct State {
    audio: speech::Audio,
}

impl StreamedProtocol<SpeechRequest, SpeechEvent, speech::Frame, State> for DeepgramSpeech {
    fn id(&self) -> &str {
        ID
    }

    fn provider(&self) -> &ProviderId {
        &self.provider
    }

    fn unsupported(&self) -> &[&'static str] {
        &["voice", "language", "instructions"]
    }

    fn body_from(&self, request: &SpeechRequest, _mode: Mode) -> Result<Body, AiError> {
        let identity = identity();
        if request.timestamps == Some(true) {
            return Err(identity.unsupported("media.timestamps", format!("{NAME} does not return timestamps")));
        }
        if let Some(format) = request.format.as_deref()
            && format_mapping(format).is_none()
            && request.provider_options.as_ref().and_then(|options| options.get("encoding")).is_none()
        {
            return Err(identity.unsupported(
                "media.format",
                format!("{NAME} has no encoding for format \"{format}\"; pass providerOptions.encoding"),
            ));
        }
        let mut base = Map::new();
        base.insert("text".to_string(), Value::String(request.text.clone()));
        let http = http_body(request);
        let value = crate::json::merge_records([&base, &http]).unwrap_or_default();
        Ok(Body::Json { value, query: Some(query_parameters(request)?) })
    }

    fn frames(
        &self,
        bytes: ByteStream,
        _context: &DecodeContext<SpeechRequest>,
    ) -> Pin<Box<dyn Stream<Item = Result<speech::Frame, AiError>> + Send>> {
        speech::bytes(bytes)
    }

    fn initial(&self) -> State {
        State::default()
    }

    fn step(&self, state: State, frame: speech::Frame) -> Result<(State, Vec<SpeechEvent>), AiError> {
        let chunk = match frame {
            speech::Frame::Bytes(bytes) => bytes,
            speech::Frame::Text(text) => text.into_bytes(),
        };
        let mut state = state;
        let events = speech::delta(&mut state.audio, &chunk);
        Ok((state, events))
    }

    fn finish(&self, state: &State, context: &ResponseContext<SpeechRequest>) -> Result<Vec<SpeechEvent>, AiError> {
        let identity = identity();
        let headers = &context.http.headers;
        let media_type = headers.get("content-type").cloned();
        let format = audio_format(&context.request);
        let headerless = format.encoding.as_deref().and_then(headerless_encoding);
        let container = format.container.clone().or_else(|| headerless.map(|_| "wav".to_string()));
        let mut output = match (&container, headerless) {
            (Some(container), Some((encoding, default_rate))) if container == "none" => {
                let sample_rate = speech::sample_rate(media_type.as_deref())
                    .or_else(|| {
                        context
                            .request
                            .provider_options
                            .as_ref()
                            .and_then(|options| options.get("sampleRate"))
                            .and_then(Value::as_number)
                            .cloned()
                    })
                    .unwrap_or_else(|| Number::from(default_rate));
                speech::pcm(encoding, Some(sample_rate), media_type.clone())
            }
            _ => speech::SpeechOutput {
                media_type: media_type.clone(),
                info: Some(MediaInfo {
                    format: Some(if container.as_deref() == Some("wav") {
                        "wav".to_string()
                    } else {
                        format.encoding.clone().unwrap_or_else(|| "mp3".to_string())
                    }),
                    ..MediaInfo::default()
                }),
                usage: None,
                provider_metadata: None,
                detail: None,
            },
        };
        output.usage =
            speech::header_usage(HeaderUsageKind::Characters, headers.get("dg-char-count").map(String::as_str));
        output.provider_metadata = provider_metadata(
            headers.get("dg-request-id").map(String::as_str),
            headers.get("dg-model-name").map(String::as_str),
        );
        speech::finish(&identity, &state.audio, output)
    }
}

struct AudioFormat {
    encoding: Option<String>,
    container: Option<String>,
}

fn audio_format(request: &SpeechRequest) -> AudioFormat {
    let mapped = request.format.as_deref().and_then(format_mapping);
    AudioFormat {
        encoding: string_option(request.provider_options.as_ref(), "encoding")
            .or_else(|| mapped.map(|(encoding, _)| encoding.to_string())),
        container: string_option(request.provider_options.as_ref(), "container")
            .or_else(|| mapped.and_then(|(_, container)| container.map(str::to_string))),
    }
}

fn format_mapping(format: &str) -> Option<(&'static str, Option<&'static str>)> {
    match format {
        "mp3" => Some(("mp3", None)),
        "wav" => Some(("linear16", Some("wav"))),
        "pcm" => Some(("linear16", Some("none"))),
        "opus" => Some(("opus", None)),
        "flac" => Some(("flac", None)),
        "aac" => Some(("aac", None)),
        _ => None,
    }
}

fn headerless_encoding(encoding: &str) -> Option<(PcmEncoding, i64)> {
    match encoding {
        "linear16" => Some((PcmEncoding::S16le, 24000)),
        "mulaw" => Some((PcmEncoding::Mulaw, 8000)),
        "alaw" => Some((PcmEncoding::Alaw, 8000)),
        _ => None,
    }
}

fn query_parameters(request: &SpeechRequest) -> Result<Query, AiError> {
    let options = request.provider_options.as_ref();
    let mut values = Map::new();
    if let Some(options) = options {
        for (key, value) in options {
            if matches!(key.as_str(), "encoding" | "container" | "sampleRate" | "bitRate") {
                continue;
            }
            values.insert(key.clone(), value.clone());
        }
    }
    values.insert("model".to_string(), Value::String(request.model.id.clone()));
    let format = audio_format(request);
    if let Some(encoding) = format.encoding {
        values.insert("encoding".to_string(), Value::String(encoding));
    }
    if let Some(container) = format.container {
        values.insert("container".to_string(), Value::String(container));
    }
    if let Some(sample_rate) = options.and_then(|options| options.get("sampleRate")) {
        values.insert("sample_rate".to_string(), sample_rate.clone());
    }
    if let Some(bit_rate) = options.and_then(|options| options.get("bitRate")) {
        values.insert("bit_rate".to_string(), bit_rate.clone());
    }
    if let Some(speed) = &request.speed {
        values.insert("speed".to_string(), Value::Number(speed.clone()));
    }
    values.into_iter().map(|(key, value)| query_value(&key, value)).collect()
}

fn query_value(key: &str, value: Value) -> Result<(String, QueryValue), AiError> {
    let invalid = || shared::invalid_request(format!("{ID} cannot send \"{key}\" as a query parameter"));
    let value = match value {
        Value::String(value) => QueryValue::One(value),
        Value::Number(value) => QueryValue::One(value.to_string()),
        Value::Bool(value) => QueryValue::One(value.to_string()),
        Value::Array(items) => QueryValue::Many(
            items
                .into_iter()
                .map(|item| match item {
                    Value::String(value) => Ok(value),
                    Value::Number(value) => Ok(value.to_string()),
                    Value::Bool(value) => Ok(value.to_string()),
                    _ => Err(invalid()),
                })
                .collect::<Result<Vec<String>, AiError>>()?,
        ),
        _ => return Err(invalid()),
    };
    Ok((key.to_string(), value))
}

fn provider_metadata(request_id: Option<&str>, model_name: Option<&str>) -> Option<ProviderMetadata> {
    if request_id.is_none() && model_name.is_none() {
        return None;
    }
    let mut deepgram = Map::new();
    if let Some(request_id) = request_id {
        deepgram.insert("requestId".to_string(), Value::String(request_id.to_string()));
    }
    if let Some(model_name) = model_name {
        deepgram.insert("modelName".to_string(), Value::String(model_name.to_string()));
    }
    let mut metadata = Map::new();
    metadata.insert("deepgram".to_string(), Value::Object(deepgram));
    Some(metadata)
}

fn string_option(options: Option<&Map<String, Value>>, key: &str) -> Option<String> {
    options.and_then(|options| options.get(key)).and_then(Value::as_str).map(str::to_string)
}

fn identity() -> Identity {
    Identity { id: ID.to_string(), name: NAME.to_string(), provider: ProviderId::from(PROVIDER) }
}

fn http_body(request: &SpeechRequest) -> Map<String, Value> {
    request.http.as_ref().and_then(|http| http.body.clone()).unwrap_or_default()
}
