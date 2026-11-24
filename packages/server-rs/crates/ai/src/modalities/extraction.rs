use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ExecutionContext;
use crate::media::route::{InlineInput, InlineProtocol, InlineRoute, MediaEndpoint, MediaRequest};
use crate::route::auth::Auth;
use crate::route::executor::{BoxFuture, RequestExecutor};
use crate::schema::{
    AiError, AiErrorReason, HttpOptions, ModelId, ProviderId, ProviderMetadata, ProviderOptions, RequestOptions,
    RequestTimeout, StringMap,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionResponse {
    pub answers: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<crate::schema::Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_metadata: Option<ProviderMetadata>,
}

#[derive(Clone)]
pub struct ExtractionModel {
    pub id: ModelId,
    pub provider: ProviderId,
    pub route: Arc<InlineRoute<ExtractionRequest, ExtractionResponse>>,
    pub transport: Option<RequestOptions>,
    pub provider_options: Option<ProviderOptions>,
}

#[derive(Clone)]
pub struct ExtractionRequest {
    pub model: ExtractionModel,
    pub input: String,
    pub fields: Map<String, Value>,
    pub reference_date: Option<String>,
    pub provider_options: Option<ProviderOptions>,
    pub headers: Option<StringMap>,
    pub timeout: Option<RequestTimeout>,
    pub http: Option<HttpOptions>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionRequestInput {
    pub input: String,
    pub fields: Map<String, Value>,
    #[serde(default)]
    pub reference_date: Option<String>,
    #[serde(default)]
    pub provider_options: Option<ProviderOptions>,
    #[serde(default)]
    pub headers: Option<StringMap>,
    #[serde(default)]
    pub timeout: Option<RequestTimeout>,
    #[serde(default)]
    pub http: Option<HttpOptions>,
}

impl ExtractionRequestInput {
    pub fn with_model(self, model: ExtractionModel) -> ExtractionRequest {
        ExtractionRequest {
            model,
            input: self.input,
            fields: self.fields,
            reference_date: self.reference_date,
            provider_options: self.provider_options,
            headers: self.headers,
            timeout: self.timeout,
            http: self.http,
        }
    }
}

impl MediaRequest for ExtractionRequest {
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
            input: self.input.clone(),
            fields: self.fields.clone(),
            reference_date: self.reference_date.clone(),
            provider_options,
            headers,
            timeout,
            http,
        }
    }
}

impl ExtractionModel {
    #[allow(clippy::too_many_arguments)]
    pub fn inline(
        id: &str,
        provider: ProviderId,
        protocol: InlineProtocol<ExtractionRequest, ExtractionResponse>,
        endpoint: MediaEndpoint<ExtractionRequest>,
        auth: Auth,
        headers: Option<StringMap>,
        transport: Option<RequestOptions>,
        provider_options: Option<ProviderOptions>,
    ) -> Self {
        let route = crate::media::route::inline(InlineInput {
            protocol,
            endpoint,
            auth,
            headers,
            provider_metadata_key: provider.clone(),
        });
        Self { id: id.to_string(), provider, route: Arc::new(route), transport, provider_options }
    }
}

pub struct ExtractionClient;

impl ExtractionClient {
    pub fn generate(
        request: ExtractionRequest,
        executor: &RequestExecutor,
        context: &ExecutionContext,
    ) -> BoxFuture<Result<ExtractionResponse, AiError>> {
        let executor = executor.clone();
        let context = context.clone();
        Box::pin(async move {
            if request.fields.is_empty() {
                return Err(AiError {
                    reason: AiErrorReason::InvalidRequest {
                        message: "Extraction fields must not be empty".to_string(),
                        body: None,
                        http: None,
                        parameter: None,
                        classification: None,
                    },
                });
            }
            let route = request.model.route.clone();
            route.generate(request, &executor, &context).await
        })
    }
}
