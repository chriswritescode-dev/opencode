use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::media::protocol::{Identity, TextResponse};
use crate::modalities::embedding::{
    EmbeddingInputType, EmbeddingRequest, EmbeddingResponse, EmbeddingUsage, EmbeddingValue,
};
use crate::protocols::utils::gemini_generate_content;
use crate::route::embedding::EmbeddingProtocol;
use crate::schema::{AiError, ProviderMetadata};

pub const ID: &str = "google-embed-content";
pub const NAME: &str = "Google embedContent";
pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
pub const MAX_BATCH: usize = 100;

pub struct GoogleEmbedContent;

pub struct GoogleVertexEmbedContent;

impl EmbeddingProtocol for GoogleEmbedContent {
    fn id(&self) -> &str {
        ID
    }

    fn name(&self) -> &str {
        NAME
    }

    fn body_from(&self, request: &EmbeddingRequest) -> Result<Map<String, Value>, AiError> {
        let contents = request.values.iter().map(content).collect::<Result<Vec<_>, _>>()?;
        let fields = fields(request);
        let requests = contents
            .iter()
            .map(|item| {
                let mut entry = Map::new();
                entry.insert("model".to_string(), Value::String(format!("models/{}", request.model.id)));
                entry.insert("content".to_string(), item.clone());
                for (key, value) in &fields {
                    entry.insert(key.clone(), value.clone());
                }
                Value::Object(entry)
            })
            .collect::<Vec<_>>();
        let mut body = Map::new();
        body.insert("requests".to_string(), Value::Array(requests));
        if let Some(overlay) = request.http.as_ref().and_then(|http| http.body.as_ref()) {
            body = crate::json::merge_objects(&body, overlay);
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
        let output: BatchOutput = identity.decode_json(text)?;
        Ok(EmbeddingResponse {
            embeddings: output.embeddings.into_iter().map(|item| item.values).collect(),
            usage: usage(output.usage_metadata.as_ref()),
            provider_metadata: Some(provider_metadata(provider_metadata_key, output.usage_metadata.as_ref(), None)),
        })
    }
}

impl EmbeddingProtocol for GoogleVertexEmbedContent {
    fn id(&self) -> &str {
        "google-vertex-embed-content"
    }

    fn name(&self) -> &str {
        "Google Vertex embedContent"
    }

    fn body_from(&self, request: &EmbeddingRequest) -> Result<Map<String, Value>, AiError> {
        let contents = request.values.iter().map(content).collect::<Result<Vec<_>, _>>()?;
        let fields = fields(request);
        let mut body = Map::new();
        if let Some(content) = contents.into_iter().next() {
            body.insert("content".to_string(), content);
        }
        if !fields.is_empty() {
            body.insert("embedContentConfig".to_string(), Value::Object(fields));
        }
        if let Some(overlay) = request.http.as_ref().and_then(|http| http.body.as_ref()) {
            body = crate::json::merge_objects(&body, overlay);
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
        let output: SingleOutput = identity.decode_json(text)?;
        Ok(EmbeddingResponse {
            embeddings: vec![output.embedding.values],
            usage: usage(output.usage_metadata.as_ref()),
            provider_metadata: Some(provider_metadata(
                provider_metadata_key,
                output.usage_metadata.as_ref(),
                output.truncated,
            )),
        })
    }
}

fn content(value: &EmbeddingValue) -> Result<Value, AiError> {
    match value {
        EmbeddingValue::Text(text) => Ok(json!({ "parts": [{ "text": text }] })),
        EmbeddingValue::Media(asset) => {
            let part = gemini_generate_content::media_part(NAME, asset)?;
            Ok(json!({ "parts": [part] }))
        }
    }
}

fn fields(request: &EmbeddingRequest) -> Map<String, Value> {
    let mut fields = Map::new();
    if let Some(input_type) = request.input_type {
        let task_type = match input_type {
            EmbeddingInputType::Query => "RETRIEVAL_QUERY",
            EmbeddingInputType::Document => "RETRIEVAL_DOCUMENT",
        };
        fields.insert("taskType".to_string(), Value::String(task_type.to_string()));
    }
    if let Some(dimensions) = request.dimensions {
        fields.insert("outputDimensionality".to_string(), json!(dimensions));
    }
    if let Some(options) = &request.provider_options {
        for (key, value) in options {
            fields.insert(key.clone(), value.clone());
        }
    }
    fields
}

fn usage(metadata: Option<&Map<String, Value>>) -> Option<EmbeddingUsage> {
    metadata
        .and_then(|metadata| metadata.get("promptTokenCount"))
        .and_then(Value::as_u64)
        .map(|input_tokens| EmbeddingUsage { input_tokens: Some(input_tokens) })
}

fn provider_metadata(
    key: &str,
    usage_metadata: Option<&Map<String, Value>>,
    truncated: Option<bool>,
) -> ProviderMetadata {
    let mut metadata = Map::new();
    if let Some(usage_metadata) = usage_metadata {
        metadata.insert("usageMetadata".to_string(), Value::Object(usage_metadata.clone()));
    }
    if let Some(truncated) = truncated {
        metadata.insert("truncated".to_string(), Value::Bool(truncated));
    }
    let mut provider_metadata = ProviderMetadata::new();
    provider_metadata.insert(key.to_string(), Value::Object(metadata));
    provider_metadata
}

#[derive(Deserialize)]
struct Values {
    values: Vec<f64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BatchOutput {
    embeddings: Vec<Values>,
    #[serde(default)]
    usage_metadata: Option<Map<String, Value>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SingleOutput {
    embedding: Values,
    #[serde(default)]
    usage_metadata: Option<Map<String, Value>>,
    #[serde(default)]
    truncated: Option<bool>,
}
