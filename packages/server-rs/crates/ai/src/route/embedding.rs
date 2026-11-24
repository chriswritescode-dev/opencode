use std::sync::Arc;

use futures_util::{StreamExt, stream};
use serde_json::{Map, Value};

use crate::ExecutionContext;
use crate::media::protocol::{Body, DecodeContext, Identity, TextResponse};
use crate::media::route::{InlineInput, InlineProtocol, InlineRoute, MediaEndpoint};
use crate::modalities::embedding::{EmbeddingModel, EmbeddingRequest, EmbeddingResponse, EmbeddingUsage};
use crate::route::auth::Auth;
use crate::route::executor::{BoxFuture, RequestExecutor};
use crate::schema::{AiError, ProviderId, ProviderMetadata, RequestOptions, StringMap};

const CONCURRENCY: usize = 4;

pub trait EmbeddingProtocol: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn body_from(&self, request: &EmbeddingRequest) -> Result<Map<String, Value>, AiError>;
    fn decode(
        &self,
        identity: &Identity,
        text: &TextResponse,
        request: &EmbeddingRequest,
        provider_metadata_key: &str,
    ) -> Result<EmbeddingResponse, AiError>;
}

pub struct EmbeddingRouteInput {
    pub id: String,
    pub provider: ProviderId,
    pub protocol: Arc<dyn EmbeddingProtocol>,
    pub endpoint: MediaEndpoint<EmbeddingRequest>,
    pub auth: Auth,
    pub headers: Option<StringMap>,
    pub provider_metadata_key: Option<String>,
    pub options: Option<RequestOptions>,
    pub max_values: Option<usize>,
}

#[derive(Clone)]
pub struct EmbeddingRoute {
    pub id: String,
    pub provider: ProviderId,
    pub provider_metadata_key: String,
    pub protocol: Arc<dyn EmbeddingProtocol>,
    pub inline: InlineRoute<EmbeddingRequest, EmbeddingResponse>,
    pub options: Option<RequestOptions>,
    pub max_values: Option<usize>,
    pub headers: Option<StringMap>,
}

impl EmbeddingRoute {
    pub fn make(input: EmbeddingRouteInput) -> Self {
        let EmbeddingRouteInput {
            id,
            provider,
            protocol,
            endpoint,
            auth,
            headers,
            provider_metadata_key,
            options,
            max_values,
        } = input;
        let provider_metadata_key = provider_metadata_key.unwrap_or_else(|| provider.clone());
        let body_protocol = protocol.clone();
        let body_from = Arc::new(move |request: &EmbeddingRequest| {
            let value = body_protocol.body_from(request)?;
            Ok(Body::Json { value, query: None })
        });
        let decode_protocol = protocol.clone();
        let identity_id = protocol.id().to_string();
        let identity_name = protocol.name().to_string();
        let identity_provider = provider.clone();
        let decode = Arc::new(move |text: &TextResponse, context: &DecodeContext<EmbeddingRequest>| {
            let identity =
                Identity { id: identity_id.clone(), name: identity_name.clone(), provider: identity_provider.clone() };
            decode_protocol.decode(&identity, text, &context.request, &context.provider_metadata_key)
        });
        let inline = crate::media::route::inline(InlineInput {
            protocol: InlineProtocol {
                id: protocol.id().to_string(),
                provider: provider.clone(),
                unsupported: Vec::new(),
                body_from,
                decode,
            },
            endpoint,
            auth,
            headers: headers.clone(),
            provider_metadata_key: provider_metadata_key.clone(),
        });
        Self { id, provider, provider_metadata_key, protocol, inline, options, max_values, headers }
    }

    pub fn model(&self, id: &str, provider_options: Option<crate::schema::ProviderOptions>) -> EmbeddingModel {
        EmbeddingModel {
            id: id.to_string(),
            provider: self.provider.clone(),
            route: Arc::new(self.clone()),
            transport: self.options.clone(),
            provider_options,
        }
    }

    pub fn generate(
        &self,
        request: EmbeddingRequest,
        executor: &RequestExecutor,
        context: &ExecutionContext,
    ) -> BoxFuture<Result<EmbeddingResponse, AiError>> {
        let inline = self.inline.clone();
        let max_values = self.max_values;
        let executor = executor.clone();
        let context = context.clone();
        Box::pin(async move {
            let Some(max_values) = max_values.filter(|max_values| request.values.len() > *max_values) else {
                return inline.generate(request, &executor, &context).await;
            };
            let chunks = request.values.chunks(max_values).map(|values| values.to_vec()).collect::<Vec<_>>();
            let responses = stream::iter(chunks.into_iter().map(|values| {
                let executor = executor.clone();
                let context = context.clone();
                let mut chunk = request.clone();
                chunk.values = values;
                inline.generate(chunk, &executor, &context)
            }))
            .buffered(CONCURRENCY)
            .collect::<Vec<Result<EmbeddingResponse, AiError>>>()
            .await;
            let responses = responses.into_iter().collect::<Result<Vec<_>, _>>()?;
            Ok(merge(responses))
        })
    }
}

fn merge(responses: Vec<EmbeddingResponse>) -> EmbeddingResponse {
    let embeddings = responses.iter().flat_map(|response| response.embeddings.iter().cloned()).collect();
    let tokens = responses.iter().try_fold(0u64, |total, response| {
        response.usage.as_ref().and_then(|usage| usage.input_tokens).map(|count| total + count)
    });
    let mut keys: Vec<String> = Vec::new();
    for key in responses.iter().flat_map(|response| response.provider_metadata.iter().flat_map(Map::keys)) {
        if !keys.iter().any(|existing| existing == key) {
            keys.push(key.clone());
        }
    }
    let provider_metadata = (!keys.is_empty()).then(|| {
        keys.into_iter()
            .map(|key| {
                let values = responses
                    .iter()
                    .map(|response| {
                        response
                            .provider_metadata
                            .as_ref()
                            .and_then(|metadata| metadata.get(&key))
                            .cloned()
                            .unwrap_or_else(|| Value::Object(Map::new()))
                    })
                    .collect::<Vec<_>>();
                let mut entry = Map::new();
                entry.insert("responses".to_string(), Value::Array(values));
                (key, Value::Object(entry))
            })
            .collect::<ProviderMetadata>()
    });
    EmbeddingResponse {
        embeddings,
        usage: tokens.map(|input_tokens| EmbeddingUsage { input_tokens: Some(input_tokens) }),
        provider_metadata,
    }
}
