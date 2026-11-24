use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::ExecutionContext;
use crate::media::route::MediaRequest;
use crate::route::embedding::EmbeddingRoute;
use crate::route::executor::{BoxFuture, RequestExecutor};
use crate::schema::{
    AiError, AiErrorReason, HttpOptions, MediaAsset, ModelId, ProviderId, ProviderMetadata, ProviderOptions,
    RequestOptions, RequestTimeout, StringMap,
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)]
pub enum EmbeddingValue {
    Text(String),
    Media(MediaAsset),
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EmbeddingInputType {
    Query,
    Document,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingUsage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingResponse {
    pub embeddings: Vec<Vec<f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<EmbeddingUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_metadata: Option<ProviderMetadata>,
}

#[derive(Clone)]
pub struct EmbeddingModel {
    pub id: ModelId,
    pub provider: ProviderId,
    pub route: Arc<EmbeddingRoute>,
    pub transport: Option<RequestOptions>,
    pub provider_options: Option<ProviderOptions>,
}

#[derive(Clone)]
pub struct EmbeddingRequest {
    pub model: EmbeddingModel,
    pub values: Vec<EmbeddingValue>,
    pub dimensions: Option<i64>,
    pub input_type: Option<EmbeddingInputType>,
    pub provider_options: Option<ProviderOptions>,
    pub headers: Option<StringMap>,
    pub timeout: Option<RequestTimeout>,
    pub http: Option<HttpOptions>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingRequestInput {
    pub values: Vec<EmbeddingValue>,
    #[serde(default)]
    pub dimensions: Option<i64>,
    #[serde(default)]
    pub input_type: Option<EmbeddingInputType>,
    #[serde(default)]
    pub provider_options: Option<ProviderOptions>,
    #[serde(default)]
    pub headers: Option<StringMap>,
    #[serde(default)]
    pub timeout: Option<RequestTimeout>,
    #[serde(default)]
    pub http: Option<HttpOptions>,
}

impl EmbeddingRequestInput {
    pub fn with_model(self, model: EmbeddingModel) -> EmbeddingRequest {
        EmbeddingRequest {
            model,
            values: self.values,
            dimensions: self.dimensions,
            input_type: self.input_type,
            provider_options: self.provider_options,
            headers: self.headers,
            timeout: self.timeout,
            http: self.http,
        }
    }
}

impl MediaRequest for EmbeddingRequest {
    fn id(&self) -> &str {
        &self.model.id
    }

    fn provider(&self) -> &ProviderId {
        &self.model.provider
    }

    fn headers(&self) -> Option<&StringMap> {
        self.headers.as_ref()
    }

    fn timeout(&self) -> Option<&RequestTimeout> {
        self.timeout.as_ref()
    }

    fn http(&self) -> Option<&HttpOptions> {
        self.http.as_ref()
    }

    fn provider_options(&self) -> Option<&ProviderOptions> {
        self.provider_options.as_ref()
    }

    fn model_transport(&self) -> Option<&RequestOptions> {
        self.model.transport.as_ref()
    }

    fn model_provider_options(&self) -> Option<&ProviderOptions> {
        self.model.provider_options.as_ref()
    }

    fn resolved(
        &self,
        headers: Option<StringMap>,
        timeout: Option<RequestTimeout>,
        http: Option<HttpOptions>,
        provider_options: Option<ProviderOptions>,
    ) -> Self {
        Self {
            model: self.model.clone(),
            values: self.values.clone(),
            dimensions: self.dimensions,
            input_type: self.input_type,
            provider_options,
            headers,
            timeout,
            http,
        }
    }
}

pub struct EmbeddingClient;

impl EmbeddingClient {
    pub fn generate(
        request: EmbeddingRequest,
        executor: &RequestExecutor,
        context: &ExecutionContext,
    ) -> BoxFuture<Result<EmbeddingResponse, AiError>> {
        let route = request.model.route.clone();
        let executor = executor.clone();
        let context = context.clone();
        Box::pin(async move {
            let response = route.generate(request.clone(), &executor, &context).await?;
            if response.embeddings.len() == request.values.len() {
                return Ok(response);
            }
            Err(AiError {
                reason: AiErrorReason::InvalidProviderOutput {
                    message: format!("Expected {} embeddings, got {}", request.values.len(), response.embeddings.len()),
                    body: None,
                    http: None,
                    classification: None,
                    route: Some(route.id.clone()),
                },
            })
        })
    }
}
