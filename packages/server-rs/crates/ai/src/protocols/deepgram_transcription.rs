use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Map, Number, Value};

use crate::media::protocol::{Body, DecodeContext, Identity, Query, TextResponse};
use crate::media::route::InlineProtocol;
use crate::modalities::transcription::{
    MediaUsage, TranscriptionRequest, TranscriptionResponse, TranscriptionSegment, TranscriptionWord,
};
use crate::protocols::shared;
use crate::protocols::utils::media_input;
use crate::protocols::utils::speaker_turns;
use crate::schema::{AiError, ProviderId};

pub const ID: &str = "deepgram-transcription";
pub const NAME: &str = "Deepgram";
pub const DEFAULT_BASE_URL: &str = "https://api.deepgram.com";
pub const PATH: &str = "/v1/listen";

const PROVIDER: &str = "deepgram";

pub fn protocol() -> InlineProtocol<TranscriptionRequest, TranscriptionResponse> {
    InlineProtocol {
        id: ID.to_string(),
        provider: ProviderId::from(PROVIDER),
        unsupported: vec!["prompt", "speakers"],
        body_from: Arc::new(body),
        decode: Arc::new(decode),
    }
}

fn query(request: &TranscriptionRequest) -> Result<Query, AiError> {
    let mut base = Map::new();
    base.insert("model".to_string(), Value::String(request.model.id.clone()));
    base.insert("smart_format".to_string(), Value::Bool(true));
    if let Some(language) = &request.language {
        base.insert("language".to_string(), Value::String(language.clone()));
    } else {
        base.insert("detect_language".to_string(), Value::Bool(true));
    }
    if request.diarize == Some(true) {
        base.insert("diarize_model".to_string(), Value::String("latest".to_string()));
    }
    if request.diarize == Some(true) || request.timestamps.as_deref() == Some("segment") {
        base.insert("utterances".to_string(), Value::Bool(true));
    }
    let merged = {
        let mut items: Vec<&Map<String, Value>> = vec![&base];
        if let Some(options) = request.provider_options.as_ref() {
            items.push(options);
        }
        crate::json::merge_records(items)
    };
    media_input::query(ID, &merged.unwrap_or(base))
}

fn body(request: &TranscriptionRequest) -> Result<Body, AiError> {
    if let Some(url) = shared::media_url(&request.audio) {
        let mut base = Map::new();
        base.insert("url".to_string(), Value::String(url.to_string()));
        let value = {
            let mut items: Vec<&Map<String, Value>> = vec![&base];
            if let Some(body) = request.http.as_ref().and_then(|http| http.body.as_ref()) {
                items.push(body);
            }
            crate::json::merge_records(items)
        }
        .unwrap_or(base);
        return Ok(Body::json(value, Some(query(request)?)));
    }
    if request.http.as_ref().and_then(|http| http.body.as_ref()).is_some() {
        return Err(shared::invalid_request(format!(
            "{NAME} sends inline audio as the raw body, so http.body cannot apply"
        )));
    }
    let bytes = media_input::inline_bytes(ID, &request.audio).map_err(shared::invalid_request)?;
    Ok(Body::binary(bytes, shared::media_type(&request.audio), Some(query(request)?)))
}

#[derive(Deserialize)]
struct ListenResponse {
    #[serde(default)]
    metadata: Option<Metadata>,
    results: Results,
}

#[derive(Deserialize)]
struct Metadata {
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    duration: Option<Number>,
}

#[derive(Deserialize)]
struct Results {
    channels: Vec<Channel>,
    #[serde(default)]
    utterances: Option<Vec<Utterance>>,
}

#[derive(Deserialize)]
struct Channel {
    #[serde(default)]
    alternatives: Option<Vec<Alternative>>,
    #[serde(default)]
    detected_language: Option<String>,
}

#[derive(Deserialize)]
struct Alternative {
    transcript: String,
    #[serde(default)]
    words: Option<Vec<Word>>,
}

#[derive(Deserialize)]
struct Utterance {
    start: Number,
    end: Number,
    transcript: String,
    #[serde(default)]
    speaker: Option<Number>,
    #[serde(default)]
    words: Option<Vec<Word>>,
}

#[derive(Deserialize)]
struct Word {
    word: String,
    start: Number,
    end: Number,
    #[serde(default)]
    confidence: Option<Number>,
    #[serde(default)]
    speaker: Option<Number>,
    #[serde(default)]
    punctuated_word: Option<String>,
}

fn decode(
    response: &TextResponse,
    _context: &DecodeContext<TranscriptionRequest>,
) -> Result<TranscriptionResponse, AiError> {
    let output = identity().output(response);
    let listen: ListenResponse = output.json()?;
    let ListenResponse { metadata, results } = listen;
    let duration = metadata.as_ref().and_then(|metadata| metadata.duration.as_ref()).map(number);
    let request_id = metadata.and_then(|metadata| metadata.request_id);
    let Some(channel) = results.channels.into_iter().next() else {
        return Err(output.invalid(format!("{NAME} returned no transcript")));
    };
    let language = channel.detected_language.map(|language| language.to_lowercase());
    let Some(alternative) = channel.alternatives.and_then(|alternatives| alternatives.into_iter().next()) else {
        return Err(output.invalid(format!("{NAME} returned no transcript")));
    };
    let segments = results
        .utterances
        .map(|utterances| utterances.into_iter().flat_map(utterance_segments).collect::<Vec<TranscriptionSegment>>());
    let words = alternative.words.map(|words| words.iter().map(transcription_word).collect::<Vec<TranscriptionWord>>());
    let usage = duration.clone().map(|seconds| MediaUsage::Seconds { seconds });
    let provider_metadata = request_id.map(|request_id| {
        let mut deepgram = Map::new();
        deepgram.insert("requestId".to_string(), Value::String(request_id));
        let mut metadata = Map::new();
        metadata.insert("deepgram".to_string(), Value::Object(deepgram));
        metadata
    });
    Ok(TranscriptionResponse {
        text: alternative.transcript,
        segments,
        words,
        language,
        duration_seconds: duration,
        usage,
        provider_metadata,
    })
}

fn utterance_segments(utterance: Utterance) -> Vec<TranscriptionSegment> {
    let Utterance { start, end, transcript, speaker: utterance_speaker, words } = utterance;
    let Some(words) = words.filter(|words| !words.is_empty()) else {
        return vec![TranscriptionSegment {
            text: transcript,
            start_seconds: number(&start),
            end_seconds: number(&end),
            speaker: speaker_string(utterance_speaker.as_ref()),
        }];
    };
    speaker_turns::group(words, |word| word.speaker.clone())
        .into_iter()
        .filter_map(|turn| {
            let first = turn.first()?;
            let last = turn.last()?;
            Some(TranscriptionSegment {
                text: turn.iter().map(word_text).collect::<Vec<String>>().join(" "),
                start_seconds: number(&first.start),
                end_seconds: number(&last.end),
                speaker: speaker_string(first.speaker.as_ref()),
            })
        })
        .collect()
}

fn transcription_word(word: &Word) -> TranscriptionWord {
    TranscriptionWord {
        text: word_text(word),
        start_seconds: number(&word.start),
        end_seconds: number(&word.end),
        speaker: speaker_string(word.speaker.as_ref()),
        confidence: word.confidence.as_ref().map(number),
    }
}

fn word_text(word: &Word) -> String {
    word.punctuated_word.clone().unwrap_or_else(|| word.word.clone())
}

fn speaker_string(value: Option<&Number>) -> Option<String> {
    value.map(|value| number(value).to_string())
}

fn number(value: &Number) -> Number {
    shared::number_from_f64(value.as_f64().unwrap_or_default()).unwrap_or_else(|| value.clone())
}

fn identity() -> Identity {
    Identity { id: ID.to_string(), name: NAME.to_string(), provider: ProviderId::from(PROVIDER) }
}
