use std::pin::Pin;

use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

use crate::media::multipart::{AppendFieldsOptions, Form, RepeatArrays, append_fields};
use crate::media::protocol::{Body, DecodeContext, Identity};
use crate::media::route::{Mode, ResponseContext, StreamedProtocol};
use crate::modalities::image::token_usage;
use crate::modalities::transcription::{
    MediaUsage, TranscriptionEvent, TranscriptionRequest, TranscriptionSegment, TranscriptionWord,
};
use crate::protocols::shared;
use crate::protocols::utils::media_input;
use crate::protocols::utils::media_type;
use crate::provider_error::{ProviderFailure, classify_provider_failure};
use crate::route::executor::ByteStream;
use crate::route::framing;
use crate::schema::{AiError, ProviderId};

pub const ID: &str = "openai-transcription";
pub const NAME: &str = "OpenAI Transcription";
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
pub const PATH: &str = "/audio/transcriptions";

const PROVIDER: &str = "openai";
const RESERVED_FORM_FIELDS: [&str; 7] =
    ["file", "model", "prompt", "language", "response_format", "timestamp_granularities", "stream"];

pub struct OpenAiTranscription {
    provider: ProviderId,
}

pub fn protocol() -> OpenAiTranscription {
    OpenAiTranscription { provider: ProviderId::from(PROVIDER) }
}

pub enum Frame {
    Text(String),
    Document(String),
}

#[derive(Default)]
pub struct State {
    segments: Vec<TranscriptionSegment>,
    transcript: Option<Transcript>,
}

#[derive(Deserialize)]
struct Language {
    code: String,
}

#[derive(Deserialize)]
struct Segment {
    text: String,
    start: Number,
    end: Number,
    #[serde(default)]
    speaker: Option<String>,
}

#[derive(Deserialize)]
struct Word {
    word: String,
    start: Number,
    end: Number,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Usage {
    Tokens {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_tokens: Option<Number>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_tokens: Option<Number>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total_tokens: Option<Number>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_token_details: Option<TokenDetails>,
    },
    Duration {
        seconds: Number,
    },
}

#[derive(Deserialize, Serialize)]
struct TokenDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    audio_tokens: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text_tokens: Option<Number>,
}

#[derive(Deserialize)]
struct Transcript {
    text: String,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    languages: Option<Vec<Language>>,
    #[serde(default)]
    duration: Option<Number>,
    #[serde(default)]
    segments: Option<Vec<Segment>>,
    #[serde(default)]
    words: Option<Vec<Word>>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct DeltaEvent {
    delta: String,
}

#[derive(Deserialize)]
struct ErrorEvent {
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    error: Option<ErrorMessage>,
}

#[derive(Deserialize)]
struct ErrorMessage {
    #[serde(default)]
    message: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LanguageField {
    Language,
    Languages,
}

struct Capabilities {
    stream: bool,
    timestamps: &'static [&'static str],
    diarize: bool,
    language_field: LanguageField,
}

const TRANSCRIBE: Capabilities =
    Capabilities { stream: true, timestamps: &[], diarize: false, language_field: LanguageField::Language };

fn capabilities(model: &str) -> Capabilities {
    if model.starts_with("whisper") {
        return Capabilities { stream: false, timestamps: &["segment", "word"], ..TRANSCRIBE };
    }
    if model.contains("diarize") {
        return Capabilities { timestamps: &["segment"], diarize: true, ..TRANSCRIBE };
    }
    if model.starts_with("gpt-transcribe") {
        return Capabilities { language_field: LanguageField::Languages, ..TRANSCRIBE };
    }
    TRANSCRIBE
}

fn streams_events(request: &TranscriptionRequest, mode: Mode) -> bool {
    mode == Mode::Stream && capabilities(&request.model.id).stream
}

impl StreamedProtocol<TranscriptionRequest, TranscriptionEvent, Frame, State> for OpenAiTranscription {
    fn id(&self) -> &str {
        ID
    }

    fn provider(&self) -> &ProviderId {
        &self.provider
    }

    fn unsupported(&self) -> &[&'static str] {
        &["speakers"]
    }

    fn body_from(&self, request: &TranscriptionRequest, mode: Mode) -> Result<Body, AiError> {
        let identity = identity();
        let model = capabilities(&request.model.id);
        validate(request, &identity, &model)?;
        let content_type = shared::media_type(&request.audio);
        let Some(extension) = media_type::extension(&content_type) else {
            return Err(shared::invalid_request(format!(
                "{NAME} cannot name a {content_type} upload; send mp3, mp4, m4a, wav, webm, ogg, or flac audio"
            )));
        };
        let bytes = media_input::inline_bytes(ID, &request.audio).map_err(shared::invalid_request)?;
        let response_format = if model.diarize {
            Some("diarized_json")
        } else if request.timestamps.as_deref().is_none_or(|timestamps| timestamps == "none") {
            None
        } else {
            Some("verbose_json")
        };
        let mut form = Form::new();
        form.append_file("file", bytes, Some(format!("audio.{extension}")), Some(content_type));
        let mut fields = Map::new();
        fields.insert("model".to_string(), Value::String(request.model.id.clone()));
        if model.language_field == LanguageField::Language {
            insert(&mut fields, "language", request.language.clone().map(Value::String));
        } else if let Some(language) = &request.language {
            fields.insert("languages".to_string(), Value::Array(vec![Value::String(language.clone())]));
        }
        insert(&mut fields, "prompt", request.prompt.clone().map(Value::String));
        insert(&mut fields, "response_format", response_format.map(|value| Value::String(value.to_string())));
        if response_format == Some("verbose_json")
            && let Some(timestamps) = &request.timestamps
        {
            fields.insert("timestamp_granularities".to_string(), Value::Array(vec![Value::String(timestamps.clone())]));
        }
        if model.diarize {
            fields.insert("chunking_strategy".to_string(), Value::String("auto".to_string()));
        }
        if streams_events(request, mode) {
            fields.insert("stream".to_string(), Value::Bool(true));
        }
        let overlay = overlay(request);
        append_fields(
            &mut form,
            &fields,
            AppendFieldsOptions {
                overlay: Some(&overlay),
                reserved: &RESERVED_FORM_FIELDS,
                repeat_arrays: Some(RepeatArrays::Brackets),
            },
        );
        Ok(Body::Multipart(form))
    }

    fn frames(
        &self,
        bytes: ByteStream,
        context: &DecodeContext<TranscriptionRequest>,
    ) -> Pin<Box<dyn Stream<Item = Result<Frame, AiError>> + Send>> {
        if streams_events(&context.request, context.mode) {
            Box::pin(framing::sse_frames(bytes, None, false).map(|frame| frame.map(Frame::Text)))
        } else {
            Box::pin(framing::document_frames(bytes).map(|frame| frame.map(Frame::Document)))
        }
    }

    fn initial(&self) -> State {
        State::default()
    }

    fn step(&self, state: State, frame: Frame) -> Result<(State, Vec<TranscriptionEvent>), AiError> {
        match frame {
            Frame::Text(frame) => self.on_event(state, &frame),
            Frame::Document(document) => {
                let transcript = parse_transcript(&document)?;
                let mut state = state;
                state.transcript = Some(transcript);
                Ok((state, Vec::new()))
            }
        }
    }

    fn finish(
        &self,
        state: &State,
        _context: &ResponseContext<TranscriptionRequest>,
    ) -> Result<Vec<TranscriptionEvent>, AiError> {
        let Some(transcript) = state.transcript.as_ref() else {
            return Err(identity().incomplete());
        };
        let segments: Vec<TranscriptionSegment> = match &transcript.segments {
            Some(segments) => segments.iter().map(segment).collect(),
            None => state.segments.clone(),
        };
        let words = transcript.words.as_ref().map(|words| {
            words
                .iter()
                .map(|word| TranscriptionWord {
                    text: word.word.clone(),
                    start_seconds: number(&word.start),
                    end_seconds: number(&word.end),
                    speaker: None,
                    confidence: None,
                })
                .collect::<Vec<TranscriptionWord>>()
        });
        let language = transcript
            .language
            .clone()
            .or_else(|| {
                transcript.languages.as_ref().and_then(|languages| languages.first()).map(|value| value.code.clone())
            })
            .map(|language| language.to_lowercase());
        Ok(vec![TranscriptionEvent::Finish {
            text: transcript.text.clone(),
            segments: if segments.is_empty() { None } else { Some(segments) },
            words,
            language,
            duration_seconds: transcript.duration.as_ref().map(number),
            usage: usage(transcript.usage.as_ref()),
            provider_metadata: None,
        }])
    }
}

impl OpenAiTranscription {
    fn on_event(&self, state: State, frame: &str) -> Result<(State, Vec<TranscriptionEvent>), AiError> {
        let identity = identity();
        let invalid = || identity.frame_error(format!("{NAME} sent an invalid stream event"), Some(frame.to_string()));
        let value: Value = serde_json::from_str(frame).map_err(|_| invalid())?;
        let object = value.as_object().ok_or_else(invalid)?;
        let kind = object.get("type").and_then(Value::as_str).ok_or_else(invalid)?.to_string();
        match kind.as_str() {
            "transcript.text.delta" => {
                let event: DeltaEvent = serde_json::from_value(value).map_err(|_| invalid())?;
                if event.delta.is_empty() {
                    return Ok((state, Vec::new()));
                }
                Ok((state, vec![TranscriptionEvent::TextDelta { delta: event.delta }]))
            }
            "transcript.text.segment" => {
                let value: Segment = serde_json::from_value(value).map_err(|_| invalid())?;
                let next = segment(&value);
                let mut state = state;
                state.segments.push(next.clone());
                Ok((state, vec![TranscriptionEvent::Segment { segment: next }]))
            }
            "transcript.text.done" => {
                let transcript: Transcript = serde_json::from_value(value).map_err(|_| invalid())?;
                Ok((State { transcript: Some(transcript), ..state }, Vec::new()))
            }
            "error" => {
                let event: ErrorEvent = serde_json::from_value(value).map_err(|_| invalid())?;
                let message = event
                    .message
                    .or_else(|| event.error.and_then(|error| error.message))
                    .unwrap_or_else(|| "unknown error".to_string());
                Err(AiError {
                    reason: classify_provider_failure(ProviderFailure {
                        message: format!("{NAME} stream failed: {message}"),
                        raw_body: Some(frame.to_string()),
                        ..ProviderFailure::default()
                    }),
                })
            }
            _ => Ok((state, Vec::new())),
        }
    }
}

fn validate(request: &TranscriptionRequest, identity: &Identity, model: &Capabilities) -> Result<(), AiError> {
    let id = &request.model.id;
    if request.diarize == Some(true) && !model.diarize {
        return Err(
            identity.unsupported("media.diarize", format!("{id} does not diarize; use gpt-4o-transcribe-diarize"))
        );
    }
    if request.prompt.is_some() && model.diarize {
        return Err(identity.unsupported("media.prompt", format!("{id} does not accept a prompt")));
    }
    if let Some(timestamps) = request.timestamps.as_deref()
        && timestamps != "none"
        && !model.timestamps.contains(&timestamps)
    {
        return Err(identity.unsupported("media.timestamps", format!("{id} does not return {timestamps} timestamps")));
    }
    Ok(())
}

fn overlay(request: &TranscriptionRequest) -> Map<String, Value> {
    let mut items: Vec<&Map<String, Value>> = Vec::new();
    if let Some(options) = request.provider_options.as_ref() {
        items.push(options);
    }
    if let Some(body) = request.http.as_ref().and_then(|http| http.body.as_ref()) {
        items.push(body);
    }
    crate::json::merge_records(items).unwrap_or_default()
}

fn parse_transcript(frame: &str) -> Result<Transcript, AiError> {
    serde_json::from_str(frame)
        .map_err(|_| identity().frame_error(format!("{NAME} sent an invalid stream event"), Some(frame.to_string())))
}

fn segment(value: &Segment) -> TranscriptionSegment {
    TranscriptionSegment {
        text: value.text.trim().to_string(),
        start_seconds: number(&value.start),
        end_seconds: number(&value.end),
        speaker: value.speaker.clone(),
    }
}

fn number(value: &Number) -> Number {
    shared::number_from_f64(value.as_f64().unwrap_or_default()).unwrap_or_else(|| value.clone())
}

fn usage(value: Option<&Usage>) -> Option<MediaUsage> {
    let value = value?;
    match value {
        Usage::Duration { seconds } => Some(MediaUsage::Seconds { seconds: number(seconds) }),
        Usage::Tokens { input_tokens, output_tokens, total_tokens, .. } => {
            let mut details = Map::new();
            details.insert("openai".to_string(), serde_json::to_value(value).unwrap_or(Value::Null));
            token_usage(
                input_tokens.as_ref().map(number),
                output_tokens.as_ref().map(number),
                total_tokens.as_ref().map(number),
                Some(details),
            )
        }
    }
}

fn identity() -> Identity {
    Identity { id: ID.to_string(), name: NAME.to_string(), provider: ProviderId::from(PROVIDER) }
}

fn insert(target: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        target.insert(key.to_string(), value);
    }
}
