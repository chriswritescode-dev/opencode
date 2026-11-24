use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Map, Number, Value};

use crate::media::multipart::{AppendFieldsOptions, Form, RepeatArrays, append_fields};
use crate::media::protocol::{Body, DecodeContext, Identity, TextResponse};
use crate::media::route::InlineProtocol;
use crate::modalities::transcription::{
    MediaUsage, TranscriptionRequest, TranscriptionResponse, TranscriptionSegment, TranscriptionWord,
};
use crate::protocols::shared;
use crate::protocols::utils::media_input;
use crate::protocols::utils::media_type;
use crate::protocols::utils::speaker_turns;
use crate::schema::{AiError, ProviderId};

pub const ID: &str = "elevenlabs-transcription";
pub const NAME: &str = "ElevenLabs Transcription";
pub const DEFAULT_BASE_URL: &str = "https://api.elevenlabs.io";
pub const PATH: &str = "/v1/speech-to-text";

const PROVIDER: &str = "elevenlabs";
const RESERVED_FORM_FIELDS: [&str; 7] =
    ["file", "cloud_storage_url", "source_url", "model_id", "language_code", "diarize", "num_speakers"];

pub fn protocol() -> InlineProtocol<TranscriptionRequest, TranscriptionResponse> {
    InlineProtocol {
        id: ID.to_string(),
        provider: ProviderId::from(PROVIDER),
        unsupported: vec!["prompt"],
        body_from: Arc::new(body),
        decode: Arc::new(decode),
    }
}

fn diarizes(request: &TranscriptionRequest) -> bool {
    request.diarize == Some(true) || request.timestamps.as_deref() == Some("segment") || request.speakers.is_some()
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

fn validate(request: &TranscriptionRequest, overlay: &Map<String, Value>) -> Result<(), AiError> {
    let identity = identity();
    if overlay.get("webhook") == Some(&Value::Bool(true)) {
        return Err(identity.unsupported("transcription.webhook", format!("{NAME} does not deliver to webhooks")));
    }
    if overlay.get("use_multi_channel") == Some(&Value::Bool(true))
        && overlay.get("multichannel_output_style").and_then(Value::as_str) != Some("combined")
    {
        return Err(identity.unsupported(
            "transcription.multichannel",
            format!(
                "{NAME} returns a single transcript; set multichannel_output_style: \"combined\" to merge channels"
            ),
        ));
    }
    if overlay.get("timestamps_granularity").and_then(Value::as_str) == Some("none")
        && (request.timestamps.as_deref() == Some("word") || diarizes(request))
    {
        return Err(identity.unsupported(
            "media.timestamps",
            format!("{NAME} cannot return word timestamps or speaker turns with timestamps_granularity: \"none\""),
        ));
    }
    Ok(())
}

fn body(request: &TranscriptionRequest) -> Result<Body, AiError> {
    let overlay = overlay(request);
    validate(request, &overlay)?;
    let mut form = Form::new();
    let url = shared::media_url(&request.audio);
    if url.is_none() {
        let content_type = shared::media_type(&request.audio);
        let extension = media_type::extension(&content_type);
        let bytes = media_input::inline_bytes(ID, &request.audio).map_err(shared::invalid_request)?;
        form.append_file(
            "file",
            bytes,
            Some(extension.map(|extension| format!("audio.{extension}")).unwrap_or_else(|| "audio".to_string())),
            Some(content_type),
        );
    }
    let mut fields = Map::new();
    fields.insert("model_id".to_string(), Value::String(request.model.id.clone()));
    if let Some(url) = url {
        fields.insert("source_url".to_string(), Value::String(url.to_string()));
    }
    if let Some(language) = &request.language {
        fields.insert("language_code".to_string(), Value::String(language.clone()));
    }
    if diarizes(request) {
        fields.insert("diarize".to_string(), Value::Bool(true));
    }
    if let Some(speakers) = request.speakers {
        fields.insert("num_speakers".to_string(), Value::Number(Number::from(speakers)));
    }
    append_fields(
        &mut form,
        &fields,
        AppendFieldsOptions {
            overlay: Some(&overlay),
            reserved: &RESERVED_FORM_FIELDS,
            repeat_arrays: Some(RepeatArrays::Key),
        },
    );
    Ok(Body::Multipart(form))
}

#[derive(Deserialize)]
struct Token {
    text: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    start: Option<Number>,
    #[serde(default)]
    end: Option<Number>,
    #[serde(default)]
    speaker_id: Option<String>,
    #[serde(default)]
    logprob: Option<Number>,
}

#[derive(Deserialize)]
struct Transcript {
    #[serde(default)]
    language_code: Option<String>,
    text: String,
    #[serde(default)]
    words: Option<Vec<Token>>,
    #[serde(default)]
    transcription_id: Option<String>,
    #[serde(default)]
    audio_duration_secs: Option<Number>,
}

fn decode(
    response: &TextResponse,
    context: &DecodeContext<TranscriptionRequest>,
) -> Result<TranscriptionResponse, AiError> {
    let output = identity().output(response);
    let transcript: Transcript = output.json()?;
    let Transcript { language_code, text, words, transcription_id, audio_duration_secs } = transcript;
    let tokens = words.unwrap_or_default();
    let duration = audio_duration_secs.as_ref().map(number);
    let segments = if diarizes(&context.request) { Some(speaker_segments(&tokens)) } else { None };
    let words = tokens.iter().filter_map(transcription_word).collect::<Vec<TranscriptionWord>>();
    let usage = duration.clone().map(|seconds| MediaUsage::Seconds { seconds });
    let provider_metadata = transcription_id.map(|transcription_id| {
        let mut elevenlabs = Map::new();
        elevenlabs.insert("transcriptionId".to_string(), Value::String(transcription_id));
        let mut metadata = Map::new();
        metadata.insert("elevenlabs".to_string(), Value::Object(elevenlabs));
        metadata
    });
    Ok(TranscriptionResponse {
        text,
        segments,
        words: Some(words),
        language: language_code.map(|language| language.to_lowercase()),
        duration_seconds: duration,
        usage,
        provider_metadata,
    })
}

fn is_timed_word(token: &Token) -> bool {
    token.kind == "word" && token.start.is_some() && token.end.is_some()
}

fn speaker_segments(tokens: &[Token]) -> Vec<TranscriptionSegment> {
    let turns = speaker_turns::group(
        tokens.iter().filter(|token| token.kind == "word" || token.kind == "spacing").collect::<Vec<&Token>>(),
        |token| token.speaker_id.clone(),
    );
    turns
        .into_iter()
        .filter_map(|turn| {
            let words = turn.iter().copied().filter(|token| is_timed_word(token)).collect::<Vec<&Token>>();
            let first = words.first()?;
            let last = words.last()?;
            let text = turn.iter().map(|token| token.text.as_str()).collect::<String>().trim().to_string();
            Some(TranscriptionSegment {
                text,
                start_seconds: first.start.as_ref().map(number)?,
                end_seconds: last.end.as_ref().map(number)?,
                speaker: turn.first()?.speaker_id.clone(),
            })
        })
        .collect()
}

fn transcription_word(word: &Token) -> Option<TranscriptionWord> {
    if !is_timed_word(word) {
        return None;
    }
    Some(TranscriptionWord {
        text: word.text.clone(),
        start_seconds: word.start.as_ref().map(number)?,
        end_seconds: word.end.as_ref().map(number)?,
        speaker: word.speaker_id.clone(),
        confidence: word
            .logprob
            .as_ref()
            .and_then(Number::as_f64)
            .and_then(|value| shared::number_from_f64(value.exp())),
    })
}

fn number(value: &Number) -> Number {
    shared::number_from_f64(value.as_f64().unwrap_or_default()).unwrap_or_else(|| value.clone())
}

fn identity() -> Identity {
    Identity { id: ID.to_string(), name: NAME.to_string(), provider: ProviderId::from(PROVIDER) }
}
