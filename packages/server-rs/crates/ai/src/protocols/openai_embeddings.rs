use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::media::protocol::{Identity, TextResponse};
use crate::modalities::embedding::{EmbeddingRequest, EmbeddingResponse, EmbeddingUsage, EmbeddingValue};
use crate::route::embedding::EmbeddingProtocol;
use crate::schema::{AiError, AiErrorReason, ProviderMetadata};

pub const ID: &str = "openai-embeddings";
pub const NAME: &str = "OpenAI Embeddings";
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
pub const PATH: &str = "/embeddings";

pub struct OpenAiEmbeddings;

impl EmbeddingProtocol for OpenAiEmbeddings {
    fn id(&self) -> &str {
        ID
    }

    fn name(&self) -> &str {
        NAME
    }

    fn body_from(&self, request: &EmbeddingRequest) -> Result<Map<String, Value>, AiError> {
        let input = request
            .values
            .iter()
            .map(|value| match value {
                EmbeddingValue::Text(text) => Ok(Value::String(text.clone())),
                EmbeddingValue::Media(_) => Err(AiError {
                    reason: AiErrorReason::UnsupportedOperation {
                        operation: "embedding.media".to_string(),
                        provider: Some(request.model.provider.clone()),
                        route: Some(request.model.route.id.clone()),
                        message: format!(
                            "{NAME} embeds text only; pass media values to a host with a multimodal embedding API"
                        ),
                        body: None,
                        http: None,
                    },
                }),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut body = Map::new();
        body.insert("encoding_format".to_string(), Value::String("float".to_string()));
        if let Some(options) = &request.provider_options {
            for (key, value) in options {
                body.insert(key.clone(), value.clone());
            }
        }
        if let Some(overlay) = request.http.as_ref().and_then(|http| http.body.as_ref()) {
            body = crate::json::merge_objects(&body, overlay);
        }
        body.insert("model".to_string(), Value::String(request.model.id.clone()));
        body.insert("input".to_string(), Value::Array(input));
        if let Some(dimensions) = request.dimensions {
            body.insert("dimensions".to_string(), json!(dimensions));
        }
        Ok(body)
    }

    fn decode(
        &self,
        identity: &Identity,
        text: &TextResponse,
        _request: &EmbeddingRequest,
        provider_metadata_key: &str,
    ) -> Result<EmbeddingResponse, AiError> {
        let output: Output = identity.decode_json(text)?;
        let mut items = output
            .data
            .into_iter()
            .enumerate()
            .map(|(position, item)| (item.index.unwrap_or(position as i64), item.embedding))
            .collect::<Vec<_>>();
        items.sort_by_key(|(index, _)| *index);
        let embeddings = items
            .into_iter()
            .map(|(_, embedding)| match embedding {
                ResponseEmbedding::Values(values) => Ok(values),
                ResponseEmbedding::Base64(value) => decode_base64(identity, &value),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let usage = output.usage.map(order_usage);
        let provider_metadata = provider_metadata(provider_metadata_key, output.model.as_ref(), usage.as_ref());
        Ok(EmbeddingResponse {
            embeddings,
            usage: usage
                .as_ref()
                .and_then(|usage| usage.get("prompt_tokens"))
                .and_then(Value::as_u64)
                .map(|input_tokens| EmbeddingUsage { input_tokens: Some(input_tokens) }),
            provider_metadata: Some(provider_metadata),
        })
    }
}

#[derive(Deserialize)]
struct Output {
    data: Vec<OutputItem>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<Map<String, Value>>,
}

#[derive(Deserialize)]
struct OutputItem {
    #[serde(default)]
    index: Option<i64>,
    embedding: ResponseEmbedding,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ResponseEmbedding {
    Values(Vec<f64>),
    Base64(String),
}

fn decode_base64(identity: &Identity, value: &str) -> Result<Vec<f64>, AiError> {
    let bytes = BASE64_STANDARD
        .decode(value)
        .map_err(|_| identity.invalid(format!("{NAME} returned an invalid base64 embedding"), None, None))?;
    if bytes.len() % 4 != 0 {
        return Err(identity.invalid(format!("{NAME} returned an invalid base64 embedding"), None, None));
    }
    Ok(bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| {
            let mut array = [0u8; 4];
            array.copy_from_slice(chunk);
            f32::from_le_bytes(array) as f64
        })
        .collect())
}

fn order_usage(usage: Map<String, Value>) -> Map<String, Value> {
    let mut ordered = Map::new();
    for key in ["prompt_tokens", "total_tokens"] {
        if let Some(value) = usage.get(key) {
            ordered.insert(key.to_string(), value.clone());
        }
    }
    for (key, value) in usage {
        if key != "prompt_tokens" && key != "total_tokens" {
            ordered.insert(key, value);
        }
    }
    ordered
}

fn provider_metadata(key: &str, model: Option<&String>, usage: Option<&Map<String, Value>>) -> ProviderMetadata {
    let mut metadata = Map::new();
    if let Some(model) = model {
        metadata.insert("model".to_string(), Value::String(model.clone()));
    }
    if let Some(usage) = usage {
        metadata.insert("usage".to_string(), Value::Object(usage.clone()));
    }
    let mut provider_metadata = ProviderMetadata::new();
    provider_metadata.insert(key.to_string(), Value::Object(metadata));
    provider_metadata
}
