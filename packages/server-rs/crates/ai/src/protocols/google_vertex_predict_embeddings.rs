use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::media::protocol::{Identity, TextResponse};
use crate::modalities::embedding::{
    EmbeddingInputType, EmbeddingRequest, EmbeddingResponse, EmbeddingUsage, EmbeddingValue,
};
use crate::route::embedding::EmbeddingProtocol;
use crate::schema::{AiError, AiErrorReason, ProviderMetadata};

pub const ID: &str = "google-vertex-predict-embeddings";
pub const NAME: &str = "Google Vertex text embeddings";
pub const MAX_INSTANCES: usize = 250;

pub struct GoogleVertexPredictEmbeddings;

impl EmbeddingProtocol for GoogleVertexPredictEmbeddings {
    fn id(&self) -> &str {
        ID
    }

    fn name(&self) -> &str {
        NAME
    }

    fn body_from(&self, request: &EmbeddingRequest) -> Result<Map<String, Value>, AiError> {
        let contents = request
            .values
            .iter()
            .map(|value| match value {
                EmbeddingValue::Text(text) => Ok(text.clone()),
                EmbeddingValue::Media(_) => Err(AiError {
                    reason: AiErrorReason::UnsupportedOperation {
                        operation: "embedding.media".to_string(),
                        provider: Some(request.model.provider.clone()),
                        route: Some(request.model.route.id.clone()),
                        message: format!(
                            "{} on Vertex :predict embeds text only; use gemini-embedding-2 for media values",
                            request.model.id
                        ),
                        body: None,
                        http: None,
                    },
                }),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut parameters = request.provider_options.clone().unwrap_or_default();
        let task_type = parameters.remove("taskType");
        let title = parameters.remove("title");
        let mut instance = Map::new();
        if let Some(task_type) = task_type {
            instance.insert("task_type".to_string(), task_type);
        } else if let Some(input_type) = request.input_type {
            let task_type = match input_type {
                EmbeddingInputType::Query => "RETRIEVAL_QUERY",
                EmbeddingInputType::Document => "RETRIEVAL_DOCUMENT",
            };
            instance.insert("task_type".to_string(), Value::String(task_type.to_string()));
        }
        if let Some(title) = title {
            instance.insert("title".to_string(), title);
        }
        let mut merged = Map::new();
        if let Some(dimensions) = request.dimensions {
            merged.insert("outputDimensionality".to_string(), json!(dimensions));
        }
        for (key, value) in parameters {
            merged.insert(key, value);
        }
        let instances = contents
            .into_iter()
            .map(|content| {
                let mut entry = Map::new();
                entry.insert("content".to_string(), Value::String(content));
                for (key, value) in &instance {
                    entry.insert(key.clone(), value.clone());
                }
                Value::Object(entry)
            })
            .collect::<Vec<_>>();
        let mut body = Map::new();
        body.insert("instances".to_string(), Value::Array(instances));
        if !merged.is_empty() {
            body.insert("parameters".to_string(), Value::Object(merged));
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
        let Output { predictions, metadata: output_metadata } = identity.decode_json(text)?;
        let statistics = predictions
            .iter()
            .map(|prediction| order_statistics(prediction.embeddings.statistics.clone().unwrap_or_default()))
            .collect::<Vec<_>>();
        let tokens = statistics
            .iter()
            .map(|statistics| statistics.get("token_count").and_then(Value::as_u64))
            .collect::<Vec<_>>();
        let embeddings = predictions.into_iter().map(|prediction| prediction.embeddings.values).collect();
        let usage = tokens
            .iter()
            .all(|token| token.is_some())
            .then(|| EmbeddingUsage { input_tokens: Some(tokens.iter().flatten().sum()) });
        let mut metadata = Map::new();
        metadata.insert("statistics".to_string(), Value::Array(statistics.into_iter().map(Value::Object).collect()));
        if let Some(output_metadata) = output_metadata {
            metadata.insert("metadata".to_string(), Value::Object(output_metadata));
        }
        let mut provider_metadata = ProviderMetadata::new();
        provider_metadata.insert(provider_metadata_key.to_string(), Value::Object(metadata));
        Ok(EmbeddingResponse { embeddings, usage, provider_metadata: Some(provider_metadata) })
    }
}

#[derive(Deserialize)]
struct Output {
    predictions: Vec<Prediction>,
    #[serde(default)]
    metadata: Option<Map<String, Value>>,
}

fn order_statistics(statistics: Map<String, Value>) -> Map<String, Value> {
    let mut ordered = Map::new();
    for key in ["token_count", "truncated"] {
        if let Some(value) = statistics.get(key) {
            ordered.insert(key.to_string(), value.clone());
        }
    }
    for (key, value) in statistics {
        if key != "token_count" && key != "truncated" {
            ordered.insert(key, value);
        }
    }
    ordered
}

#[derive(Deserialize)]
struct Prediction {
    embeddings: PredictionEmbeddings,
}

#[derive(Deserialize)]
struct PredictionEmbeddings {
    values: Vec<f64>,
    #[serde(default)]
    statistics: Option<Map<String, Value>>,
}
