use std::collections::HashMap;
use std::time::Duration;

use indexmap::IndexMap;
use opencode_ai::HttpSender;
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    ModelCapabilities, ModelCompatibility, ModelCost, ModelCostCache, ModelInfo, ModelInfoLimit, ModelInfoStatus,
    ModelInfoTime, ModelReasoningField, ModelReasoningFieldString, ModelReasoningFieldString2, ModelSettings,
    ModelVariant, ModelVariantSettings, MoneyUSDPerMillionTokens, ProviderID,
};
use serde::Deserialize;
use serde_json::Value;

use crate::provider;
use crate::provider_plugin::http::{self, HttpError};

const PROVIDER_ID: &str = "modal";

pub const PACKAGE: &str = "@opencode/ai/providers/openai-compatible";

const TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RemoteModel {
    pub id: String,
    #[serde(default)]
    pub base_model_id: Option<String>,
    #[serde(default)]
    pub hugging_face_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub input_modalities: Option<Vec<String>>,
    #[serde(default)]
    pub output_modalities: Option<Vec<String>>,
    #[serde(default)]
    pub context_length: Option<f64>,
    #[serde(default)]
    pub max_output_length: Option<f64>,
    #[serde(default)]
    pub pricing: Option<Pricing>,
    #[serde(default)]
    pub supported_sampling_parameters: Option<Vec<String>>,
    #[serde(default)]
    pub supported_features: Option<Vec<String>>,
    #[serde(default)]
    pub reasoning_options: Option<Vec<ReasoningOption>>,
    #[serde(default)]
    pub interleaved: Option<Interleaved>,
}

pub type Snapshot = Vec<RemoteModel>;

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Pricing {
    #[serde(default)]
    pub prompt: Option<Price>,
    #[serde(default)]
    pub completion: Option<Price>,
    #[serde(default)]
    pub input_cache_read: Option<Price>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Price {
    Number(f64),
    Text(String),
}

impl Price {
    fn number(&self) -> Option<f64> {
        match self {
            Self::Number(value) => Some(*value),
            Self::Text(value) => {
                let value = value.trim();
                if value.is_empty() {
                    return Some(0.0);
                }
                value.parse().ok()
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ReasoningOption {
    pub r#type: ReasoningType,
    pub values: Vec<Option<String>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningType {
    Effort,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Interleaved {
    Boolean(bool),
    Object { field: InterleavedField },
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterleavedField {
    Reasoning,
    ReasoningContent,
    ReasoningDetails,
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum LoadError {
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error("invalid Modal models response: {0}")]
    Json(String),
    #[error("Modal connection has no API key")]
    MissingKey,
}

impl crate::shared_cache::Transient for LoadError {
    fn is_transient(&self) -> bool {
        matches!(self, Self::Http(error) if error.transient())
    }
}

#[derive(Deserialize)]
struct Envelope {
    data: Vec<Value>,
}

pub async fn get(
    http: &dyn HttpSender,
    base_url: &str,
    api_key: &str,
    existing: &[ModelInfo],
) -> Result<IndexMap<String, ModelInfo>, LoadError> {
    let remote = load(http, base_url, api_key).await?;
    Ok(derive(base_url, &remote, existing))
}

pub async fn load(http: &dyn HttpSender, base_url: &str, api_key: &str) -> Result<Snapshot, LoadError> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let headers = vec![("authorization".to_owned(), format!("Bearer {api_key}"))];
    let response = http::send(http, http::get(url, headers), Some(TIMEOUT)).await?;
    let envelope: Envelope =
        serde_json::from_slice(&response.body).map_err(|error| LoadError::Json(error.to_string()))?;
    Ok(envelope.data.iter().filter_map(|raw| serde_json::from_value(raw.clone()).ok()).collect())
}

pub fn derive(base_url: &str, remote: &[RemoteModel], existing: &[ModelInfo]) -> IndexMap<String, ModelInfo> {
    let templates: HashMap<&str, &ModelInfo> = existing.iter().map(|model| (model.id.as_str(), model)).collect();
    remote
        .iter()
        .map(|item| {
            let template_id =
                item.base_model_id.as_deref().or(item.hugging_face_id.as_deref()).unwrap_or(item.id.as_str());
            let template = templates.get(template_id).copied();
            (item.id.clone(), build(&item.id, item, base_url, template))
        })
        .collect()
}

fn build(id: &str, remote: &RemoteModel, base_url: &str, previous: Option<&ModelInfo>) -> ModelInfo {
    let cost = previous.and_then(|model| model.cost.first());
    let input_limit = previous.and_then(|model| model.limit.input);
    let mut model = provider::default_model(&ProviderID(PROVIDER_ID.to_owned()), id);
    model.model_id = remote.id.clone();
    model.name =
        remote.name.clone().or_else(|| previous.map(|model| model.name.clone())).unwrap_or_else(|| remote.id.clone());
    model.family = previous.and_then(|model| model.family.clone());
    model.compatibility = match &remote.interleaved {
        None => previous.and_then(|model| model.compatibility.clone()),
        Some(value) => compatibility(value).or_else(|| previous.and_then(|model| model.compatibility.clone())),
    };
    model.package = Some(PACKAGE.to_owned());
    model.settings = Some(settings_for(previous, base_url));
    model.headers = previous.and_then(|model| model.headers.clone());
    model.body = previous.and_then(|model| model.body.clone());
    model.capabilities = ModelCapabilities {
        tools: remote
            .supported_features
            .as_ref()
            .map(|features| features.iter().any(|feature| feature == "tools"))
            .or_else(|| previous.map(|model| model.capabilities.tools))
            .unwrap_or(true),
        freeform: None,
        input: remote
            .input_modalities
            .clone()
            .or_else(|| previous.map(|model| model.capabilities.input.clone()))
            .unwrap_or_else(|| vec!["text".to_owned()]),
        output: remote
            .output_modalities
            .clone()
            .or_else(|| previous.map(|model| model.capabilities.output.clone()))
            .unwrap_or_else(|| vec!["text".to_owned()]),
        temperature: None,
    };
    model.variants = match &remote.reasoning_options {
        Some(_) => variants(remote),
        None => previous.map(|model| model.variants.clone()).unwrap_or_default(),
    };
    model.time = previous.map(|model| model.time.clone()).unwrap_or(ModelInfoTime { released: 0.0 });
    model.cost = vec![ModelCost {
        tier: None,
        input: price(
            remote.pricing.as_ref().and_then(|pricing| pricing.prompt.as_ref()),
            cost.map(|cost| cost.input).unwrap_or(MoneyUSDPerMillionTokens(0.0)),
        ),
        output: price(
            remote.pricing.as_ref().and_then(|pricing| pricing.completion.as_ref()),
            cost.map(|cost| cost.output).unwrap_or(MoneyUSDPerMillionTokens(0.0)),
        ),
        cache: ModelCostCache {
            read: price(
                remote.pricing.as_ref().and_then(|pricing| pricing.input_cache_read.as_ref()),
                cost.map(|cost| cost.cache.read).unwrap_or(MoneyUSDPerMillionTokens(0.0)),
            ),
            write: cost.map(|cost| cost.cache.write).unwrap_or(MoneyUSDPerMillionTokens(0.0)),
        },
    }];
    model.status = previous.map(|model| model.status).unwrap_or(ModelInfoStatus::Active);
    model.enabled = previous.map(|model| model.enabled).unwrap_or(true);
    model.limit = ModelInfoLimit {
        context: limit(remote.context_length, previous.map(|model| model.limit.context).unwrap_or(0.0)),
        input: input_limit,
        output: limit(remote.max_output_length, previous.map(|model| model.limit.output).unwrap_or(0.0)),
    };
    model
}

fn settings_for(previous: Option<&ModelInfo>, base_url: &str) -> ModelSettings {
    let base = previous.and_then(|model| model.settings.as_ref()).map(provider::json_object);
    let mut overlay = crate::aisdk_native::JsonMap::new();
    overlay.insert("baseURL".to_owned(), Value::String(base_url.to_owned()));
    overlay.insert("provider".to_owned(), Value::String(PROVIDER_ID.to_owned()));
    provider::decode_object(crate::aisdk_native::merge_overlay(base.as_ref(), Some(&overlay)).unwrap_or_default())
        .unwrap_or_else(empty_settings)
}

fn empty_settings() -> ModelSettings {
    ModelSettings { compaction: None, cache: None, encoding: None, rest: Default::default() }
}

fn price(value: Option<&Price>, fallback: MoneyUSDPerMillionTokens) -> MoneyUSDPerMillionTokens {
    let Some(value) = value else {
        return fallback;
    };
    let Some(parsed) = value.number().map(|number| number * 1_000_000.0).filter(|number| number.is_finite()) else {
        return fallback;
    };
    MoneyUSDPerMillionTokens(parsed)
}

fn limit(value: Option<f64>, fallback: f64) -> f64 {
    let parsed = value.unwrap_or(fallback).trunc();
    if parsed.is_finite() && parsed >= 0.0 { parsed } else { fallback }
}

fn compatibility(value: &Interleaved) -> Option<ModelCompatibility> {
    let Interleaved::Object { field } = value else {
        return None;
    };
    Some(ModelCompatibility {
        reasoning_field: Some(reasoning_field(*field)),
        require_reasoning: None,
        max_tokens_field: None,
        require_finish_reason: None,
        require_assistant_after_tool: None,
        supports_prompt_cache_key: None,
        stream_usage: None,
        cache_format: None,
        supports_thinking_block_binding: None,
        supports_effort_updates: None,
        media_input: None,
    })
}

fn reasoning_field(field: InterleavedField) -> ModelReasoningField {
    match field {
        InterleavedField::Reasoning => ModelReasoningField::String(ModelReasoningFieldString::Reasoning),
        InterleavedField::ReasoningContent => {
            ModelReasoningField::String2(ModelReasoningFieldString2::ReasoningContent)
        }
        InterleavedField::ReasoningDetails => ModelReasoningField::String4("reasoning_details".to_owned()),
    }
}

fn variants(remote: &RemoteModel) -> Vec<ModelVariant> {
    let mut seen: IndexMap<String, ModelVariant> = IndexMap::new();
    for option in remote.reasoning_options.iter().flatten() {
        for value in &option.values {
            let effort = value.clone().unwrap_or_else(|| "none".to_owned());
            if seen.contains_key(&effort) {
                continue;
            }
            seen.insert(
                effort.clone(),
                ModelVariant {
                    id: effort.clone(),
                    settings: Some(ModelVariantSettings {
                        compaction: None,
                        encoding: None,
                        rest: JsonMap(vec![("reasoningEffort".to_owned(), Value::String(effort))]),
                    }),
                    headers: None,
                    body: None,
                },
            );
        }
    }
    seen.into_values().collect()
}
