use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value, json};

use crate::json;
use crate::media::protocol::{Identity, TextResponse};
use crate::modalities::decision::{
    ChoiceProbability, ChoiceValue, DecisionAnswer, DecisionQuestion, DecisionRequest, DecisionResponse, DecisionUsage,
    PredicateCriteria, ScoreProbability, normalized_number, usage_number, usage_total,
};
use crate::protocols::shared;
use crate::route::decision::DecisionProtocol;
use crate::schema::{AiError, MediaAsset, ProviderMetadata, merge_provider_options};

pub const ID: &str = "system-one";
pub const NAME: &str = "System One";
pub const DEFAULT_BASE_URL: &str = "https://api.typesafe.ai/v1";
pub const PATH: &str = "/systemone";

pub struct SystemOne;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Answer {
    Noul {
        noul: Number,
    },
    Choice {
        choice: String,
        probabilities: BTreeMap<String, Number>,
        confidence: Number,
    },
    Score {
        score: Number,
        probabilities: BTreeMap<String, Number>,
        confidence: Number,
    },
    Refusal {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Response {
    pub model: String,
    pub answers: BTreeMap<String, Answer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_metadata: Option<BTreeMap<String, Map<String, Value>>>,
}

impl DecisionProtocol for SystemOne {
    fn id(&self) -> &str {
        ID
    }

    fn name(&self) -> &str {
        NAME
    }

    fn inline_media(&self) -> bool {
        true
    }

    fn body_from(&self, request: &DecisionRequest) -> Result<Map<String, Value>, AiError> {
        let mut body = encode_body(request, "assets")?;
        body.insert("model".to_string(), Value::String(request.model.id.clone()));
        Ok(body)
    }

    fn decode(
        &self,
        identity: &Identity,
        text: &TextResponse,
        request: &DecisionRequest,
        provider_metadata_key: &str,
    ) -> Result<DecisionResponse, AiError> {
        let data: Response = identity.decode_json(text)?;
        to_response(data, request, provider_metadata_key, |message| {
            identity.invalid(message, Some(text.body.clone()), Some(text.http.clone()))
        })
    }
}

pub fn encode_body(request: &DecisionRequest, fallback: &str) -> Result<Map<String, Value>, AiError> {
    let mut body = merge_provider_options(&[
        request.provider_options.as_ref(),
        request.http.as_ref().and_then(|http| http.body.as_ref()),
    ])
    .unwrap_or_default();
    for (key, value) in place_media(request, fallback)? {
        body.insert(key, value);
    }
    let questions = encode_questions(&request.questions)?;
    body.insert("questions".to_string(), Value::Object(questions));
    Ok(body)
}

pub fn encode_questions(questions: &[DecisionQuestion]) -> Result<Map<String, Value>, AiError> {
    let mut entries = Map::new();
    for question in questions {
        entries.insert(question.name().to_string(), encode_question(question)?);
    }
    Ok(entries)
}

fn encode_question(question: &DecisionQuestion) -> Result<Value, AiError> {
    match question {
        DecisionQuestion::Predicate { instructions, criteria, .. } => {
            let mut value = Map::new();
            value.insert("type".to_string(), Value::String("noul".to_string()));
            value.insert("instructions".to_string(), Value::String(instructions.clone()));
            if let Some(criteria) = criteria {
                value.insert("criteria".to_string(), criteria_value(criteria));
            }
            Ok(Value::Object(value))
        }
        DecisionQuestion::Score { instructions, levels, .. } => {
            let criteria = levels
                .iter()
                .map(|level| match &level.description {
                    Some(description) => Value::String(format!("{}: {description}", level.label)),
                    None => Value::String(level.label.clone()),
                })
                .collect::<Vec<_>>();
            Ok(json!({ "type": "score", "instructions": instructions, "criteria": criteria }))
        }
        DecisionQuestion::Choice { name, instructions, choices } => {
            let keys = choices.iter().map(|choice| choice.value.key()).collect::<Vec<_>>();
            if keys.iter().collect::<HashSet<_>>().len() != keys.len() {
                return Err(shared::invalid_request(format!(
                    "Decision choice options are sent as string keys; \"{name}\" has a boolean and a string with the same text"
                )));
            }
            let criteria = choices
                .iter()
                .map(|choice| {
                    let description = choice.description.clone().map_or(Value::Null, Value::String);
                    (choice.value.key(), description)
                })
                .collect::<Map<String, Value>>();
            Ok(json!({ "type": "choice", "instructions": instructions, "criteria": criteria }))
        }
    }
}

fn criteria_value(criteria: &PredicateCriteria) -> Value {
    let mut value = Map::new();
    if let Some(true_value) = &criteria.true_value {
        value.insert("true".to_string(), Value::String(true_value.clone()));
    }
    if let Some(false_value) = &criteria.false_value {
        value.insert("false".to_string(), Value::String(false_value.clone()));
    }
    Value::Object(value)
}

fn place_media(request: &DecisionRequest, fallback: &str) -> Result<Map<String, Value>, AiError> {
    let media_input = request
        .model
        .compatibility
        .as_ref()
        .and_then(|compatibility| compatibility.get("mediaInput"))
        .and_then(Value::as_str)
        .unwrap_or(fallback);
    let parts = &request.input;
    let media = parts.iter().filter_map(media_of).collect::<Vec<_>>();
    let mut body = Map::new();
    if media.is_empty() {
        body.insert("state".to_string(), state_value(parts.iter().filter_map(plain).collect()));
        return Ok(body);
    }
    match media_input {
        "state" => {
            let items = parts
                .iter()
                .map(|part| match part {
                    crate::modalities::decision::DecisionPart::Media { media, filename } => {
                        state_media_item(request, media, filename.as_deref(), true)
                    }
                    other => Ok(plain(other).unwrap_or(Value::Null)),
                })
                .collect::<Result<Vec<_>, _>>()?;
            body.insert("state".to_string(), Value::Array(items));
        }
        "state.messages" => {
            let content = parts
                .iter()
                .map(|part| match part {
                    crate::modalities::decision::DecisionPart::Media { media, filename } => {
                        state_media_item(request, media, filename.as_deref(), false)
                    }
                    other => Ok(json!({ "type": "text", "text": text_of(other) })),
                })
                .collect::<Result<Vec<_>, _>>()?;
            body.insert("state".to_string(), json!({ "messages": [{ "role": "user", "content": content }] }));
        }
        "root" => {
            let unplaced = media
                .iter()
                .find(|asset| !matches!(shared::media_kind(&shared::media_type(asset)), "image" | "audio" | "video"));
            if let Some(unplaced) = unplaced {
                return Err(unsupported(request, unplaced, "images, audio and video"));
            }
            let inline =
                media.iter().map(|asset| shared::require_inline_media(NAME, asset)).collect::<Result<Vec<_>, _>>()?;
            body.insert("state".to_string(), state_value(parts.iter().filter_map(plain).collect()));
            for (kind, field) in [("image", "images"), ("audio", "audio"), ("video", "videos")] {
                let items = media
                    .iter()
                    .zip(&inline)
                    .filter(|(asset, _)| shared::media_kind(&shared::media_type(asset)) == kind)
                    .map(|(_, inline)| Value::String(inline.data_url.clone()))
                    .collect::<Vec<_>>();
                if !items.is_empty() {
                    body.insert(field.to_string(), Value::Array(items));
                }
            }
        }
        _ => {
            let unplaced = media.iter().find(|asset| shared::media_kind(&shared::media_type(asset)) != "image");
            if let Some(unplaced) = unplaced {
                return Err(unsupported(request, unplaced, "images"));
            }
            let inline =
                media.iter().map(|asset| shared::require_inline_media(NAME, asset)).collect::<Result<Vec<_>, _>>()?;
            let mut assets = Map::new();
            for (index, item) in inline.iter().enumerate() {
                let format = item.mime.strip_prefix("image/").unwrap_or(&item.mime);
                assets
                    .insert(reference(index), json!({ "type": "image", "format": format, "data_base64": item.base64 }));
            }
            let mut next = 0;
            let mut items = Vec::with_capacity(parts.len());
            for part in parts {
                match part {
                    crate::modalities::decision::DecisionPart::Media { .. } => {
                        items.push(Value::String(reference(next)));
                        next += 1;
                    }
                    other => items.extend(plain(other)),
                }
            }
            body.insert("state".to_string(), Value::Array(items));
            body.insert("assets".to_string(), Value::Object(assets));
        }
    }
    Ok(body)
}

fn state_media_item(
    request: &DecisionRequest,
    asset: &MediaAsset,
    filename: Option<&str>,
    files: bool,
) -> Result<Value, AiError> {
    let media_type = shared::media_type(asset);
    let kind = shared::media_kind(&media_type);
    if !files && !matches!(kind, "image" | "audio" | "video") {
        return Err(unsupported(request, asset, "images, audio and video"));
    }
    let inline = shared::require_inline_media(NAME, asset)?;
    match kind {
        "image" => Ok(json!({ "type": "image_url", "image_url": { "url": inline.data_url } })),
        "video" => Ok(json!({ "type": "video_url", "video_url": { "url": inline.data_url } })),
        "audio" => {
            let format = inline.mime.strip_prefix("audio/").unwrap_or(&inline.mime).to_lowercase();
            Ok(json!({
                "type": "input_audio",
                "input_audio": { "data": inline.base64, "format": audio_format(&format) },
            }))
        }
        _ => {
            let mut file = Map::new();
            file.insert("file_data".to_string(), Value::String(inline.data_url));
            if let Some(filename) = filename {
                file.insert("filename".to_string(), Value::String(filename.to_string()));
            }
            Ok(json!({ "type": "file", "file": file }))
        }
    }
}

fn audio_format(format: &str) -> &str {
    match format {
        "x-wav" | "wave" | "vnd.wave" => "wav",
        "x-m4a" => "m4a",
        "x-aiff" => "aiff",
        "x-flac" => "flac",
        other => other,
    }
}

fn reference(index: usize) -> String {
    format!("<image-{}>", index + 1)
}

fn media_of(part: &crate::modalities::decision::DecisionPart) -> Option<&MediaAsset> {
    match part {
        crate::modalities::decision::DecisionPart::Media { media, .. } => Some(media),
        _ => None,
    }
}

fn plain(part: &crate::modalities::decision::DecisionPart) -> Option<Value> {
    match part {
        crate::modalities::decision::DecisionPart::Text { text } => Some(Value::String(text.clone())),
        crate::modalities::decision::DecisionPart::Json { value } => Some(value.clone()),
        crate::modalities::decision::DecisionPart::Media { .. } => None,
    }
}

fn text_of(part: &crate::modalities::decision::DecisionPart) -> String {
    match part {
        crate::modalities::decision::DecisionPart::Text { text } => text.clone(),
        crate::modalities::decision::DecisionPart::Json { value } => json::encode_body(value),
        crate::modalities::decision::DecisionPart::Media { .. } => String::new(),
    }
}

fn state_value(items: Vec<Value>) -> Value {
    if items.len() == 1 {
        return items.into_iter().next().unwrap_or(Value::Null);
    }
    Value::Array(items)
}

fn unsupported(request: &DecisionRequest, asset: &MediaAsset, accepted: &str) -> AiError {
    shared::unsupported_operation(
        "decision.media",
        format!("{NAME} takes {accepted} here; got {}", shared::media_type(asset)),
        Some(request.model.provider.clone()),
        Some(request.model.route.id.clone()),
    )
}

pub fn to_response(
    data: Response,
    request: &DecisionRequest,
    provider_metadata_key: &str,
    invalid: impl Fn(&str) -> AiError,
) -> Result<DecisionResponse, AiError> {
    let names = request.questions.iter().map(DecisionQuestion::name).collect::<Vec<_>>();
    if data.answers.len() != names.len() || !names.iter().all(|name| data.answers.contains_key(*name)) {
        return Err(invalid("System One answers do not match the requested questions"));
    }
    let mut answers = Vec::with_capacity(request.questions.len());
    for question in &request.questions {
        let Some(answer) = data.answers.get(question.name()) else {
            return Err(invalid("System One answers do not match the requested questions"));
        };
        answers.push(to_answer(question, answer));
    }
    let usage = data.usage.as_ref().map(|usage| DecisionUsage {
        input_tokens: usage_number(usage, "input_tokens"),
        output_tokens: usage_number(usage, "output_tokens"),
        total_tokens: usage_total(
            usage.get("input_tokens").and_then(Value::as_number),
            usage.get("output_tokens").and_then(Value::as_number),
            None,
        ),
        cost: usage_number(usage, "cost"),
    });
    let mut entry = Map::new();
    entry.insert("model".to_string(), Value::String(data.model.clone()));
    if let Some(usage) = &data.usage {
        entry.insert("usage".to_string(), Value::Object(usage.clone()));
    }
    if let Some(id) = &data.id {
        entry.insert("responseId".to_string(), Value::String(id.clone()));
    }
    if let Some(provider) = &data.provider {
        entry.insert("provider".to_string(), Value::String(provider.clone()));
    }
    if let Some(extra) = data.provider_metadata.as_ref().and_then(|metadata| metadata.get(provider_metadata_key)) {
        for (key, value) in extra {
            entry.insert(key.clone(), value.clone());
        }
    }
    let mut provider_metadata = ProviderMetadata::new();
    provider_metadata.insert(provider_metadata_key.to_string(), Value::Object(entry));
    Ok(DecisionResponse { answers, usage, provider_metadata: Some(provider_metadata) })
}

pub fn to_answer(question: &DecisionQuestion, answer: &Answer) -> DecisionAnswer {
    let name = question.name().to_string();
    match answer {
        Answer::Refusal { message } => {
            DecisionAnswer::Error { name, reason: "refusal".to_string(), message: message.clone() }
        }
        Answer::Noul { noul } => DecisionAnswer::Predicate { name, probability: normalized_number(noul) },
        Answer::Choice { choice, probabilities, confidence } => {
            let choices = match question {
                DecisionQuestion::Choice { choices, .. } => choices.as_slice(),
                _ => &[],
            };
            let value = |key: &str| {
                choices
                    .iter()
                    .find(|option| option.value.key() == key)
                    .map(|option| option.value.clone())
                    .unwrap_or_else(|| ChoiceValue::String(key.to_string()))
            };
            let rank = |key: &str| choices.iter().position(|option| option.value.key() == key).unwrap_or(choices.len());
            let mut entries = probabilities.iter().collect::<Vec<_>>();
            entries.sort_by_key(|(key, _)| rank(key));
            DecisionAnswer::Choice {
                name,
                choice: value(choice),
                probabilities: entries
                    .into_iter()
                    .map(|(key, probability)| ChoiceProbability {
                        value: value(key),
                        probability: normalized_number(probability),
                    })
                    .collect(),
                confidence: normalized_number(confidence),
            }
        }
        Answer::Score { score, probabilities, confidence } => {
            let levels = match question {
                DecisionQuestion::Score { levels, .. } => levels.as_slice(),
                _ => &[],
            };
            let mut entries = probabilities
                .iter()
                .map(|(key, probability)| {
                    let value = key.parse::<i64>().unwrap_or_default();
                    let label = usize::try_from(value)
                        .ok()
                        .and_then(|index| levels.get(index))
                        .map(|level| level.label.clone())
                        .unwrap_or_else(|| key.clone());
                    (value, label, normalized_number(probability))
                })
                .collect::<Vec<_>>();
            entries.sort_by_key(|(value, _, _)| *value);
            DecisionAnswer::Score {
                name,
                score: normalized_number(score),
                probabilities: entries
                    .into_iter()
                    .map(|(value, label, probability)| ScoreProbability { value, label, probability })
                    .collect(),
                confidence: normalized_number(confidence),
            }
        }
    }
}
