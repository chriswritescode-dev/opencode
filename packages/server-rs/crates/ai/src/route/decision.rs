use std::sync::Arc;

use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{Map, Value};

use crate::ExecutionContext;
use crate::media::protocol::{Body, DecodeContext, Identity, TextResponse};
use crate::media::route::{InlineInput, InlineProtocol, InlineRoute, MediaEndpoint};
use crate::modalities::decision::{DecisionModel, DecisionPart, DecisionRequest, DecisionResponse};
use crate::protocols::utils::media_type::detect_media_type;
use crate::route::auth::Auth;
use crate::route::executor::{BoxFuture, PreparedHttp, RequestExecutor};
use crate::schema::{
    AiError, MediaAsset, MediaSource, ProviderId, ProviderOptions, RequestOptions, RequestTimeout, StringMap,
    merge_request_options,
};

pub trait DecisionProtocol: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn inline_media(&self) -> bool {
        false
    }
    fn body_from(&self, request: &DecisionRequest) -> Result<Map<String, Value>, AiError>;
    fn decode(
        &self,
        identity: &Identity,
        text: &TextResponse,
        request: &DecisionRequest,
        provider_metadata_key: &str,
    ) -> Result<DecisionResponse, AiError>;
}

pub struct DecisionRouteInput {
    pub id: String,
    pub provider: ProviderId,
    pub protocol: Arc<dyn DecisionProtocol>,
    pub endpoint: MediaEndpoint<DecisionRequest>,
    pub auth: Auth,
    pub headers: Option<StringMap>,
    pub provider_metadata_key: Option<String>,
    pub options: Option<RequestOptions>,
}

#[derive(Clone)]
pub struct DecisionRoute {
    pub id: String,
    pub provider: ProviderId,
    pub provider_metadata_key: String,
    pub protocol: Arc<dyn DecisionProtocol>,
    pub inline: InlineRoute<DecisionRequest, DecisionResponse>,
    pub options: Option<RequestOptions>,
    pub headers: Option<StringMap>,
    pub inline_media: bool,
}

impl DecisionRoute {
    pub fn make(input: DecisionRouteInput) -> Self {
        let DecisionRouteInput { id, provider, protocol, endpoint, auth, headers, provider_metadata_key, options } =
            input;
        let provider_metadata_key = provider_metadata_key.unwrap_or_else(|| provider.clone());
        let inline_media = protocol.inline_media();
        let body_protocol = protocol.clone();
        let body_from = Arc::new(move |request: &DecisionRequest| {
            let value = body_protocol.body_from(request)?;
            Ok(Body::Json { value, query: None })
        });
        let decode_protocol = protocol.clone();
        let identity_id = protocol.id().to_string();
        let identity_name = protocol.name().to_string();
        let identity_provider = provider.clone();
        let decode = Arc::new(move |text: &TextResponse, context: &DecodeContext<DecisionRequest>| {
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
        Self { id, provider, provider_metadata_key, protocol, inline, options, headers, inline_media }
    }

    pub fn model(
        &self,
        id: &str,
        compatibility: Option<Value>,
        provider_options: Option<ProviderOptions>,
    ) -> DecisionModel {
        DecisionModel {
            id: id.to_string(),
            provider: self.provider.clone(),
            route: Arc::new(self.clone()),
            transport: self.options.clone(),
            compatibility,
            provider_options,
        }
    }

    pub fn generate(
        &self,
        request: DecisionRequest,
        executor: &RequestExecutor,
        context: &ExecutionContext,
    ) -> BoxFuture<Result<DecisionResponse, AiError>> {
        let inline = self.inline.clone();
        let inline_media = self.inline_media;
        let executor = executor.clone();
        let context = context.clone();
        Box::pin(async move {
            if !inline_media {
                return inline.generate(request, &executor, &context).await;
            }
            let request = materialize(request, &executor).await?;
            inline.generate(request, &executor, &context).await
        })
    }
}

async fn materialize(mut request: DecisionRequest, executor: &RequestExecutor) -> Result<DecisionRequest, AiError> {
    let timed = executor.with_timeout(merged_timeout(&request));
    let input = std::mem::take(&mut request.input);
    let mut parts = Vec::with_capacity(input.len());
    for part in input {
        parts.push(materialize_part(part, &timed).await?);
    }
    request.input = parts;
    Ok(request)
}

fn merged_timeout(request: &DecisionRequest) -> Option<RequestTimeout> {
    let request_options = RequestOptions {
        headers: request.headers.clone(),
        timeout: request.timeout.clone(),
        http: request.http.clone(),
        ..RequestOptions::default()
    };
    merge_request_options(&[request.model.transport.as_ref(), Some(&request_options)])
        .and_then(|options| options.timeout)
}

async fn materialize_part(part: DecisionPart, executor: &RequestExecutor) -> Result<DecisionPart, AiError> {
    let DecisionPart::Media { media, filename } = part else {
        return Ok(part);
    };
    let MediaSource::Url { url, media_type, .. } = &media.source else {
        return Ok(DecisionPart::Media { media, filename });
    };
    let url = url.clone();
    let declared = media_type.clone();
    let response = executor.execute(PreparedHttp::new("GET", url, Vec::new(), Bytes::new()), None).await?;
    let mut stream = response.body;
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        bytes.extend_from_slice(&chunk?);
    }
    let media_type = declared
        .or_else(|| detect_media_type(&bytes).map(str::to_string))
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let source = MediaSource::Base64 { data: BASE64_STANDARD.encode(bytes), media_type };
    Ok(DecisionPart::Media {
        media: MediaAsset { source, info: media.info, provider_metadata: media.provider_metadata },
        filename,
    })
}
