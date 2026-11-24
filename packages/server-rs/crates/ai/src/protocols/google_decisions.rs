use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::media::protocol::{Identity, TextResponse};
use crate::modalities::decision::{
    DecisionPart, DecisionQuestion, DecisionRequest, DecisionResponse, DecisionUsage, usage_number, usage_total,
};
use crate::protocols::shared;
use crate::protocols::system_one::{self, Answer};
use crate::protocols::utils::media_input;
use crate::route::decision::DecisionProtocol;
use crate::schema::{AiError, MediaAsset, MediaSource, ProviderMetadata, ProviderOptions};

pub const ID: &str = "google-decisions";
pub const NAME: &str = "Google Decisions";
pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
pub const PATH: &str = "/decisions";

const PROVIDER: &str = "google";

pub struct GoogleDecisions;

#[derive(Deserialize)]
struct Output {
    answers: BTreeMap<String, Answer>,
    #[serde(default)]
    usage: Option<Map<String, Value>>,
}

impl DecisionProtocol for GoogleDecisions {
    fn id(&self) -> &str {
        ID
    }

    fn name(&self) -> &str {
        NAME
    }

    fn body_from(&self, request: &DecisionRequest) -> Result<Map<String, Value>, AiError> {
        let resolution = media_resolution(&request.provider_options)?;
        let mut body = Map::new();
        if let Some(options) = &request.provider_options {
            for (key, value) in options {
                if key == "mediaResolution" {
                    continue;
                }
                body.insert(key.clone(), value.clone());
            }
        }
        if let Some(overlay) = request.http.as_ref().and_then(|http| http.body.as_ref()) {
            body = crate::json::merge_objects(&body, overlay);
        }
        let mut blocks = request
            .input
            .iter()
            .map(|part| input_part(request, part, resolution.as_deref()))
            .collect::<Result<Vec<_>, _>>()?;
        separate_text(&mut blocks);
        body.insert("model".to_string(), Value::String(request.model.id.clone()));
        body.insert("input".to_string(), Value::Array(blocks));
        body.insert("questions".to_string(), Value::Object(encode_questions(&request.questions)?));
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
        let names = request.questions.iter().map(DecisionQuestion::name).collect::<Vec<_>>();
        let invalid = || {
            identity.invalid(
                "Google Decisions answers do not match the requested questions",
                Some(text.body.clone()),
                Some(text.http.clone()),
            )
        };
        if output.answers.len() != names.len() || !names.iter().all(|name| output.answers.contains_key(*name)) {
            return Err(invalid());
        }
        let mut answers = Vec::with_capacity(request.questions.len());
        for question in &request.questions {
            let Some(answer) = output.answers.get(question.name()) else {
                return Err(invalid());
            };
            answers.push(system_one::to_answer(question, answer));
        }
        let usage = output.usage.as_ref().map(|usage| DecisionUsage {
            input_tokens: usage_number(usage, "total_input_tokens"),
            output_tokens: usage_number(usage, "total_output_tokens"),
            total_tokens: usage_total(
                usage.get("total_input_tokens").and_then(Value::as_number),
                usage.get("total_output_tokens").and_then(Value::as_number),
                usage.get("total_tokens").and_then(Value::as_number),
            ),
            cost: None,
        });
        let provider_metadata = output.usage.map(|usage| {
            let mut entry = Map::new();
            entry.insert("usage".to_string(), Value::Object(usage));
            let mut metadata = ProviderMetadata::new();
            metadata.insert(provider_metadata_key.to_string(), Value::Object(entry));
            metadata
        });
        Ok(DecisionResponse { answers, usage, provider_metadata })
    }
}

fn media_resolution(provider_options: &Option<ProviderOptions>) -> Result<Option<String>, AiError> {
    let Some(options) = provider_options else {
        return Ok(None);
    };
    match options.get("mediaResolution") {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(shared::invalid_request(format!("{ID} providerOptions mediaResolution is invalid"))),
    }
}

fn encode_questions(questions: &[DecisionQuestion]) -> Result<Map<String, Value>, AiError> {
    let mut entries = system_one::encode_questions(questions)?;
    for question in questions {
        let DecisionQuestion::Predicate { name, instructions, criteria: Some(criteria) } = question else {
            continue;
        };
        let mut value = Map::new();
        value.insert("type".to_string(), Value::String("noul".to_string()));
        value.insert("instructions".to_string(), Value::String(instructions.clone()));
        let mut descriptions = Map::new();
        if let Some(true_value) = &criteria.true_value {
            descriptions.insert("true_description".to_string(), Value::String(true_value.clone()));
        }
        if let Some(false_value) = &criteria.false_value {
            descriptions.insert("false_description".to_string(), Value::String(false_value.clone()));
        }
        value.insert("criteria".to_string(), Value::Object(descriptions));
        entries.insert(name.clone(), Value::Object(value));
    }
    Ok(entries)
}

fn input_part(request: &DecisionRequest, part: &DecisionPart, resolution: Option<&str>) -> Result<Value, AiError> {
    match part {
        DecisionPart::Text { text } => Ok(json!({ "type": "text", "text": text })),
        DecisionPart::Json { value } => Ok(json!({ "type": "text", "text": crate::json::encode_body(value) })),
        DecisionPart::Media { media, .. } => media_part(request, media, resolution),
    }
}

fn separate_text(blocks: &mut [Value]) {
    let texts =
        blocks.iter().map(|block| block.get("type").and_then(Value::as_str) == Some("text")).collect::<Vec<_>>();
    for index in 1..blocks.len() {
        if !texts[index] || !texts[index - 1] {
            continue;
        }
        let Some(Value::Object(block)) = blocks.get_mut(index) else {
            continue;
        };
        let Some(Value::String(text)) = block.get_mut("text") else {
            continue;
        };
        *text = format!("\n{text}");
    }
}

fn media_part(request: &DecisionRequest, asset: &MediaAsset, resolution: Option<&str>) -> Result<Value, AiError> {
    let media_type = shared::media_type(asset);
    let lower = media_type.to_lowercase();
    let kind = shared::media_kind(&media_type);
    let block_type = match kind {
        "image" | "audio" | "video" => kind,
        _ if lower == "application/pdf" => "document",
        _ => {
            return Err(shared::unsupported_operation(
                "decision.media",
                format!("{NAME} takes images, audio, video and PDFs; got {media_type}"),
                Some(request.model.provider.clone()),
                Some(request.model.route.id.clone()),
            ));
        }
    };
    let mut block = Map::new();
    block.insert("type".to_string(), Value::String(block_type.to_string()));
    block.insert("mime_type".to_string(), Value::String(lower));
    if let Some(resolution) = resolution
        && (block_type == "image" || (block_type == "video" && resolution != "ultra_high"))
    {
        block.insert("resolution".to_string(), Value::String(resolution.to_string()));
    }
    if let Some(inline) = shared::inline_media(asset) {
        block.insert("data".to_string(), Value::String(inline.base64));
        return Ok(Value::Object(block));
    }
    if let MediaSource::Url { url, .. } = &asset.source
        && url.to_lowercase().starts_with("https://")
        && asset.headers.is_none()
    {
        block.insert("uri".to_string(), Value::String(url.clone()));
        return Ok(Value::Object(block));
    }
    if let Some(id) = media_input::ref_id(asset, PROVIDER) {
        block.insert("uri".to_string(), Value::String(id));
        return Ok(Value::Object(block));
    }
    Err(shared::invalid_request(format!(
        "{NAME} accepts inline media, public HTTPS URLs and Google Files API URIs; materialize {} media first",
        source_type(&asset.source)
    )))
}

fn source_type(source: &MediaSource) -> &'static str {
    match source {
        MediaSource::Bytes { .. } => "bytes",
        MediaSource::Base64 { .. } => "base64",
        MediaSource::Url { .. } => "url",
        MediaSource::Ref { .. } => "ref",
    }
}
