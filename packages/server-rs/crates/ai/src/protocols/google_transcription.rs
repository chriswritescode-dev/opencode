use std::pin::Pin;

use futures_util::Stream;
use serde_json::{Map, Number, Value};

use crate::media::protocol::{Body, DecodeContext, Identity};
use crate::media::route::{Mode, ResponseContext, StreamedProtocol};
use crate::modalities::image::{MediaUsage, token_usage};
use crate::modalities::transcription::{
    TranscriptionEvent, TranscriptionRequest, TranscriptionSegment, TranscriptionWord,
};
use crate::protocols::shared::number_from_f64;
use crate::protocols::utils::gemini_generate_content::{content_filtered, media_part};
use crate::route::executor::ByteStream;
use crate::schema::{AiError, AiErrorReason, ProviderId, ProviderMetadata};

pub const ID: &str = "google-transcription";
pub const NAME: &str = "Google Transcription";
pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

const PROVIDER: &str = "google";

pub struct GoogleTranscription {
    provider: ProviderId,
}

pub fn protocol() -> GoogleTranscription {
    GoogleTranscription { provider: ProviderId::from(PROVIDER) }
}

#[derive(Default)]
pub struct State {
    text: String,
    segments: Vec<TranscriptionSegment>,
    words: Vec<TranscriptionWord>,
    usage: Option<Map<String, Value>>,
    finish_reason: Option<String>,
    model_version: Option<String>,
    response_id: Option<String>,
}

struct Turn {
    text: String,
    words: Vec<TranscriptionWord>,
    segment: Option<TranscriptionSegment>,
}

pub fn path(request: &TranscriptionRequest) -> String {
    if request.mode == Mode::Stream {
        format!("/models/{}:streamGenerateContent?alt=sse", request.model.id)
    } else {
        format!("/models/{}:generateContent", request.model.id)
    }
}

impl StreamedProtocol<TranscriptionRequest, TranscriptionEvent, String, State> for GoogleTranscription {
    fn id(&self) -> &str {
        ID
    }

    fn provider(&self) -> &ProviderId {
        &self.provider
    }

    fn unsupported(&self) -> &[&'static str] {
        &["prompt", "speakers"]
    }

    fn body_from(&self, request: &TranscriptionRequest, _mode: Mode) -> Result<Body, AiError> {
        let identity = identity();
        if !request.model.id.contains("transcribe") {
            return Err(identity.unsupported(
                "transcription.model",
                format!(
                    "{} is not a transcription model; use a transcribe model such as gemini-3.5-transcribe",
                    request.model.id
                ),
            ));
        }
        let mut config = Map::new();
        if let Some(language) = &request.language {
            config.insert("languageCodes".to_string(), Value::Array(vec![Value::String(language.clone())]));
        }
        if request.diarize == Some(true)
            || request.timestamps.as_deref() == Some("word")
            || request.timestamps.as_deref() == Some("segment")
        {
            config.insert("wordTimestamp".to_string(), Value::Bool(true));
        }
        if request.diarize == Some(true) {
            config.insert("diarization".to_string(), Value::Bool(true));
        }
        let mut generation_config = Map::new();
        generation_config.insert("audioTranscriptionConfig".to_string(), Value::Object(config));
        let options = request.provider_options.clone().unwrap_or_default();
        let generation_config = crate::json::merge_records([&generation_config, &options]).unwrap_or_default();
        let mut body = Map::new();
        let contents = vec![serde_json::json!({
            "role": "user",
            "parts": [media_part(ID, &request.audio)?],
        })];
        body.insert("contents".to_string(), Value::Array(contents));
        body.insert("generationConfig".to_string(), Value::Object(generation_config));
        let http = request.http.as_ref().and_then(|http| http.body.clone()).unwrap_or_default();
        let value = crate::json::merge_records([&body, &http]).unwrap_or_default();
        Ok(Body::Json { value, query: None })
    }

    fn frames(
        &self,
        bytes: ByteStream,
        context: &DecodeContext<TranscriptionRequest>,
    ) -> Pin<Box<dyn Stream<Item = Result<String, AiError>> + Send>> {
        if context.mode == Mode::Stream {
            crate::route::framing::sse_frames(bytes, None, false)
        } else {
            crate::route::framing::document_frames(bytes)
        }
    }

    fn initial(&self) -> State {
        State::default()
    }

    fn step(&self, state: State, frame: String) -> Result<(State, Vec<TranscriptionEvent>), AiError> {
        on_chunk(state, &frame)
    }

    fn finish(
        &self,
        state: &State,
        _context: &ResponseContext<TranscriptionRequest>,
    ) -> Result<Vec<TranscriptionEvent>, AiError> {
        if state.finish_reason.is_none() {
            return Err(identity().incomplete());
        }
        Ok(vec![TranscriptionEvent::Finish {
            text: state.text.clone(),
            segments: (!state.segments.is_empty()).then(|| state.segments.clone()),
            words: (!state.words.is_empty()).then(|| state.words.clone()),
            language: None,
            duration_seconds: None,
            usage: usage(state.usage.as_ref()),
            provider_metadata: Some(provider_metadata(state)),
        }])
    }
}

fn on_chunk(state: State, frame: &str) -> Result<(State, Vec<TranscriptionEvent>), AiError> {
    let identity = identity();
    let value: Value = serde_json::from_str(frame).map_err(|_| invalid(&identity, frame))?;
    let object = value.as_object().ok_or_else(|| invalid(&identity, frame))?;
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
    let candidates = object.get("candidates").and_then(Value::as_array);
    let finish_reason = candidates
        .and_then(|candidates| candidates.first())
        .and_then(|candidate| candidate.get("finishReason"))
        .and_then(Value::as_str);
    let mut state = track(state, object);
    let mut turns = Vec::new();
    let parts = candidates
        .and_then(|candidates| candidates.first())
        .and_then(|candidate| candidate.get("content"))
        .and_then(|content| content.get("parts"))
        .and_then(Value::as_array);
    for part in parts.into_iter().flatten() {
        let Some(transcription) = part.get("audioTranscription").and_then(Value::as_object) else {
            continue;
        };
        turns.push(turn(transcription, &identity, frame)?);
    }
    let segments = turns.iter().filter_map(|turn| turn.segment.clone()).collect::<Vec<TranscriptionSegment>>();
    state.words.extend(turns.iter().flat_map(|turn| turn.words.iter().cloned()));
    state.segments.extend(segments.iter().cloned());
    let text =
        turns.iter().map(|turn| turn.text.as_str()).filter(|text| !text.is_empty()).collect::<Vec<&str>>().join(" ");
    let delta = if text.is_empty() || state.text.is_empty() { text } else { format!(" {text}") };
    if state.text.len() + delta.len() == 0 && content_filtered(finish_reason) {
        return Err(AiError {
            reason: AiErrorReason::ContentPolicy {
                message: format!("{NAME} withheld its output ({})", finish_reason.unwrap_or_default()),
                body: Some(frame.to_string()),
                http: None,
            },
        });
    }
    let mut events = Vec::new();
    if !delta.is_empty() {
        events.push(TranscriptionEvent::TextDelta { delta: delta.clone() });
    }
    events.extend(segments.into_iter().map(|segment| TranscriptionEvent::Segment { segment }));
    state.text.push_str(&delta);
    Ok((state, events))
}

fn turn(transcription: &Map<String, Value>, identity: &Identity, frame: &str) -> Result<Turn, AiError> {
    let text = transcription.get("text").and_then(Value::as_str).ok_or_else(|| invalid(identity, frame))?.to_string();
    let speaker = transcription.get("speakerLabel").and_then(Value::as_str);
    let words = match transcription.get("words") {
        None => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|word| {
                let word = word.as_object().ok_or_else(|| invalid(identity, frame))?;
                Ok(TranscriptionWord {
                    text: word.get("word").and_then(Value::as_str).ok_or_else(|| invalid(identity, frame))?.to_string(),
                    start_seconds: seconds(word.get("startOffset"), identity, frame)?,
                    end_seconds: seconds(word.get("endOffset"), identity, frame)?,
                    speaker: word.get("speakerLabel").and_then(Value::as_str).or(speaker).map(str::to_string),
                    confidence: None,
                })
            })
            .collect::<Result<Vec<_>, AiError>>()?,
        Some(_) => return Err(invalid(identity, frame)),
    };
    let segment = match (words.first(), words.last()) {
        (Some(first), Some(last)) => Some(TranscriptionSegment {
            text: text.clone(),
            start_seconds: first.start_seconds.clone(),
            end_seconds: last.end_seconds.clone(),
            speaker: speaker.map(str::to_string),
        }),
        _ => None,
    };
    Ok(Turn { text, words, segment })
}

fn seconds(value: Option<&Value>, identity: &Identity, frame: &str) -> Result<Number, AiError> {
    let text = value.and_then(Value::as_str).ok_or_else(|| invalid(identity, frame))?;
    let number = text.strip_suffix('s').unwrap_or(text).parse::<f64>().map_err(|_| invalid(identity, frame))?;
    number_from_f64(number).ok_or_else(|| invalid(identity, frame))
}

fn track(mut state: State, object: &Map<String, Value>) -> State {
    if let Some(usage) = object.get("usageMetadata").and_then(Value::as_object) {
        state.usage = Some(usage.clone());
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
    let reported = state.get("totalTokenCount").and_then(Value::as_number).cloned();
    let total = reported.clone().or_else(|| {
        if input.is_none() && output.is_none() {
            return None;
        }
        number_from_f64(
            input.as_ref().and_then(Number::as_f64).unwrap_or_default()
                + output.as_ref().and_then(Number::as_f64).unwrap_or_default(),
        )
    });
    let mut google = Map::new();
    for (key, value) in
        [("promptTokenCount", &input), ("candidatesTokenCount", &output), ("totalTokenCount", &reported)]
    {
        if let Some(value) = value {
            google.insert(key.to_string(), Value::Number(value.clone()));
        }
    }
    let mut details = Map::new();
    details.insert("google".to_string(), Value::Object(google));
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

fn invalid(identity: &Identity, frame: &str) -> AiError {
    identity.frame_error(format!("{NAME} sent an invalid stream event"), Some(frame.to_string()))
}
