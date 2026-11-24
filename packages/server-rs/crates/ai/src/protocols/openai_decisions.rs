use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Map, Number, Value, json};

use crate::media::protocol::{Identity, TextResponse};
use crate::modalities::decision::{
    ChoiceProbability, ChoiceValue, DecisionAnswer, DecisionPart, DecisionQuestion, DecisionRequest, DecisionResponse,
    DecisionUsage, ScoreProbability, normalized_number,
};
use crate::protocols::open_responses;
use crate::protocols::shared;
use crate::protocols::utils::media_input;
use crate::route::decision::DecisionProtocol;
use crate::schema::{AiError, MediaAsset, ProviderOptions};

pub const ID: &str = "openai-decisions";
pub const NAME: &str = "OpenAI Decisions";
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
pub const PATH: &str = "/decisions";

pub struct OpenAiDecisions;

#[derive(Default)]
struct Options {
    prompt_cache_key: Option<String>,
    prompt_cache_options: Option<Value>,
    safety_identifier: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Answer {
    Predicate {
        name: String,
        probability: Number,
    },
    Choice {
        name: String,
        choice: ChoiceValue,
        probabilities: Vec<ChoiceProbability>,
        confidence: Number,
    },
    Score {
        name: String,
        score: Number,
        probabilities: Vec<ScoreProbability>,
        confidence: Number,
    },
    Refusal {
        name: String,
        #[serde(default)]
        message: Option<String>,
    },
}

#[derive(Deserialize)]
struct Output {
    model: String,
    answers: Vec<Answer>,
    #[serde(default)]
    usage: Option<Map<String, Value>>,
}

impl DecisionProtocol for OpenAiDecisions {
    fn id(&self) -> &str {
        ID
    }

    fn name(&self) -> &str {
        NAME
    }

    fn body_from(&self, request: &DecisionRequest) -> Result<Map<String, Value>, AiError> {
        let options = decode_options(&request.provider_options)?;
        let mut body = Map::new();
        if let Some(key) = options.prompt_cache_key {
            body.insert("prompt_cache_key".to_string(), Value::String(key));
        }
        if let Some(options) = options.prompt_cache_options {
            body.insert("prompt_cache_options".to_string(), options);
        }
        if let Some(identifier) = options.safety_identifier {
            body.insert("safety_identifier".to_string(), Value::String(identifier));
        }
        if let Some(native) = &request.provider_options {
            for (key, value) in native {
                if matches!(key.as_str(), "promptCacheKey" | "promptCacheOptions" | "safetyIdentifier") {
                    continue;
                }
                body.insert(key.clone(), value.clone());
            }
        }
        if let Some(overlay) = request.http.as_ref().and_then(|http| http.body.as_ref()) {
            body = crate::json::merge_objects(&body, overlay);
        }
        let content = request.input.iter().map(|part| input_part(request, part)).collect::<Result<Vec<_>, _>>()?;
        body.insert("model".to_string(), Value::String(request.model.id.clone()));
        body.insert("input".to_string(), json!([{ "role": "user", "content": content }]));
        body.insert("questions".to_string(), Value::Array(request.questions.iter().map(question_value).collect()));
        Ok(body)
    }

    fn decode(
        &self,
        identity: &Identity,
        text: &TextResponse,
        request: &DecisionRequest,
        provider_metadata_key: &str,
    ) -> Result<DecisionResponse, AiError> {
        let output: Output = identity.decode_json(text)?;
        let answers = output.answers.into_iter().map(answer).collect::<Vec<_>>();
        let mut by_name = HashMap::new();
        for answer in &answers {
            by_name.insert(answer.name().to_string(), answer.clone());
        }
        let ordered =
            request.questions.iter().filter_map(|question| by_name.get(question.name()).cloned()).collect::<Vec<_>>();
        let answers = if ordered.len() == answers.len() { ordered } else { answers };
        let raw_usage = output.usage.as_ref().map(|usage| Value::Object(usage.clone()));
        let usage =
            open_responses::map_usage(raw_usage.as_ref(), provider_metadata_key, &[]).map(|usage| DecisionUsage {
                input_tokens: usage.input_tokens.map(Number::from),
                output_tokens: usage.output_tokens.map(Number::from),
                total_tokens: usage.total_tokens.map(Number::from),
                cost: usage.cost,
            });
        let mut entry = Map::new();
        entry.insert("model".to_string(), Value::String(output.model));
        if let Some(usage) = output.usage {
            entry.insert("usage".to_string(), Value::Object(usage));
        }
        let mut provider_metadata = Map::new();
        provider_metadata.insert(provider_metadata_key.to_string(), Value::Object(entry));
        Ok(DecisionResponse { answers, usage, provider_metadata: Some(provider_metadata) })
    }
}

fn decode_options(provider_options: &Option<ProviderOptions>) -> Result<Options, AiError> {
    let Some(options) = provider_options else {
        return Ok(Options::default());
    };
    let prompt_cache_key = match options.get("promptCacheKey") {
        None => None,
        Some(Value::String(key)) => Some(key.clone()),
        Some(_) => return Err(invalid_option("promptCacheKey")),
    };
    let prompt_cache_options = match options.get("promptCacheOptions") {
        None => None,
        Some(Value::Object(entries)) => {
            for key in ["mode", "ttl"] {
                if let Some(value) = entries.get(key)
                    && !value.is_string()
                {
                    return Err(invalid_option("promptCacheOptions"));
                }
            }
            Some(Value::Object(entries.clone()))
        }
        Some(_) => return Err(invalid_option("promptCacheOptions")),
    };
    let safety_identifier = match options.get("safetyIdentifier") {
        None => None,
        Some(Value::String(identifier)) => Some(identifier.clone()),
        Some(_) => return Err(invalid_option("safetyIdentifier")),
    };
    Ok(Options { prompt_cache_key, prompt_cache_options, safety_identifier })
}

fn invalid_option(field: &str) -> AiError {
    shared::invalid_request(format!("{NAME} providerOptions {field} is invalid"))
}

fn input_part(request: &DecisionRequest, part: &DecisionPart) -> Result<Value, AiError> {
    match part {
        DecisionPart::Text { text } => Ok(json!({ "type": "input_text", "text": text })),
        DecisionPart::Json { value } => Ok(json_part(value)),
        DecisionPart::Media { media, .. } => media_part(request, media),
    }
}

fn json_part(value: &Value) -> Value {
    if value.is_object() {
        return json!({ "type": "input_json", "data": value });
    }
    if let Value::Array(items) = value
        && items.iter().all(Value::is_object)
    {
        return json!({ "type": "input_json", "data": value });
    }
    json!({ "type": "input_text", "text": crate::json::encode_body(value) })
}

fn media_part(request: &DecisionRequest, asset: &MediaAsset) -> Result<Value, AiError> {
    let media_type = shared::media_type(asset);
    let kind = shared::media_kind(&media_type);
    if kind != "image" {
        return Err(shared::unsupported_operation(
            "decision.media",
            format!("{NAME} takes images only; got {kind} media"),
            Some(request.model.provider.clone()),
            Some(request.model.route.id.clone()),
        ));
    }
    Ok(json!({
        "type": "input_image",
        "image_url": media_input::media_reference(asset, None, &format!("{NAME} image input"))
            .map(media_input::MediaReference::value)
            .map_err(shared::invalid_request)?
    }))
}

fn question_value(question: &DecisionQuestion) -> Value {
    let mut value = Map::new();
    match question {
        DecisionQuestion::Predicate { name, instructions, criteria } => {
            value.insert("name".to_string(), Value::String(name.clone()));
            value.insert("type".to_string(), Value::String("predicate".to_string()));
            let mut meanings = Vec::new();
            if let Some(criteria) = criteria {
                if let Some(true_value) = &criteria.true_value {
                    meanings.push(format!("True means: {true_value}"));
                }
                if let Some(false_value) = &criteria.false_value {
                    meanings.push(format!("False means: {false_value}"));
                }
            }
            let instructions = if meanings.is_empty() {
                instructions.clone()
            } else {
                format!("{instructions}\n\n{}", meanings.join("\n"))
            };
            value.insert("instructions".to_string(), Value::String(instructions));
        }
        DecisionQuestion::Choice { name, instructions, choices } => {
            value.insert("name".to_string(), Value::String(name.clone()));
            value.insert("type".to_string(), Value::String("choice".to_string()));
            value.insert("instructions".to_string(), Value::String(instructions.clone()));
            value.insert("choices".to_string(), json!(choices));
        }
        DecisionQuestion::Score { name, instructions, levels } => {
            value.insert("name".to_string(), Value::String(name.clone()));
            value.insert("type".to_string(), Value::String("score".to_string()));
            value.insert("instructions".to_string(), Value::String(instructions.clone()));
            value.insert("levels".to_string(), json!(levels));
        }
    }
    Value::Object(value)
}

fn answer(answer: Answer) -> DecisionAnswer {
    match answer {
        Answer::Predicate { name, probability } => {
            DecisionAnswer::Predicate { name, probability: normalized_number(&probability) }
        }
        Answer::Choice { name, choice, probabilities, confidence } => DecisionAnswer::Choice {
            name,
            choice,
            probabilities: probabilities
                .into_iter()
                .map(|item| ChoiceProbability { value: item.value, probability: normalized_number(&item.probability) })
                .collect(),
            confidence: normalized_number(&confidence),
        },
        Answer::Score { name, score, probabilities, confidence } => DecisionAnswer::Score {
            name,
            score: normalized_number(&score),
            probabilities: probabilities
                .into_iter()
                .map(|item| ScoreProbability {
                    value: item.value,
                    label: item.label,
                    probability: normalized_number(&item.probability),
                })
                .collect(),
            confidence: normalized_number(&confidence),
        },
        Answer::Refusal { name, message } => DecisionAnswer::Error { name, reason: "refusal".to_string(), message },
    }
}
