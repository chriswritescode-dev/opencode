use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value;

use crate::prepare::tool_updates::ToolUpdateEncoding;
use crate::route::auth::{Auth, AuthDescriptor};
use crate::route::endpoint::{Endpoint, PathPart};
use crate::route::protocol::Protocol;
use crate::schema::{
    AiError, AiErrorReason, LlmRequest, ModelId, ProviderId, RequestOptions, RouteId, SanitizerKind, StringMap,
    merge_request_options,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RouteTransport {
    Http,
    #[serde(rename = "openai-responses-channel")]
    OpenAiResponsesChannel,
}

pub type HeaderFactory = Arc<dyn Fn(&LlmRequest) -> Vec<(String, String)> + Send + Sync>;

#[derive(Clone)]
pub struct Route {
    pub id: RouteId,
    pub protocol: Arc<dyn Protocol>,
    pub provider_metadata_key: Option<String>,
    pub endpoint: Endpoint,
    pub auth: Auth,
    pub headers: Option<HeaderFactory>,
    pub transport: RouteTransport,
    pub options: Option<RequestOptions>,
}

#[derive(Clone)]
pub struct Model {
    pub id: ModelId,
    pub provider: ProviderId,
    pub route: Route,
    pub options: Option<RequestOptions>,
    pub compatibility: Option<Value>,
    pub capabilities: Option<Value>,
}

#[derive(Clone)]
pub struct ResolvedRequest {
    pub request: LlmRequest,
    pub model: Model,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelDescriptor {
    pub id: String,
    pub provider: String,
    pub route: RouteDescriptor,
    pub endpoint: EndpointDescriptor,
    #[serde(default)]
    pub options: Option<RequestOptions>,
    #[serde(default)]
    pub model_options: Option<RequestOptions>,
    #[serde(default)]
    pub compatibility: Option<Value>,
    #[serde(default)]
    pub capabilities: Option<Value>,
    pub transport: RouteTransport,
    pub auth: AuthDescriptor,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteDescriptor {
    pub id: String,
    pub protocol: String,
    #[serde(default)]
    pub provider_metadata_key: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EndpointDescriptor {
    #[serde(default, rename = "baseURL")]
    pub base_url: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub query: Option<StringMap>,
}

#[derive(Default)]
pub struct ProtocolRegistry {
    protocols: BTreeMap<String, Arc<dyn Protocol>>,
}

impl ProtocolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, protocol: Arc<dyn Protocol>) {
        self.protocols.insert(protocol.id().to_string(), protocol);
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn Protocol>> {
        self.protocols.get(id).cloned()
    }

    pub fn builtin() -> Self {
        Self::new()
    }
}

impl Model {
    pub fn from_descriptor(descriptor: ModelDescriptor, registry: &ProtocolRegistry) -> Result<Model, AiError> {
        let ModelDescriptor {
            id,
            provider,
            route,
            endpoint,
            options,
            model_options,
            compatibility,
            capabilities,
            transport,
            auth,
        } = descriptor;
        let protocol = registry.get(&route.protocol).ok_or_else(|| missing_protocol(&route, &provider))?;
        let path = match endpoint.path {
            Some(path) => PathPart::Static(path),
            None => PathPart::Default,
        };
        let route_options = options;
        let model_options = merge_request_options(&[route_options.as_ref(), model_options.as_ref()]);
        Ok(Model {
            id,
            provider,
            route: Route {
                id: route.id,
                protocol,
                provider_metadata_key: route.provider_metadata_key,
                endpoint: Endpoint { base_url: endpoint.base_url, path, query: endpoint.query.unwrap_or_default() },
                auth: auth.into(),
                headers: None,
                transport,
                options: route_options,
            },
            options: model_options,
            compatibility,
            capabilities,
        })
    }

    pub fn compatibility_str(&self, key: &str) -> Option<&str> {
        self.compatibility.as_ref().and_then(|value| value.get(key)).and_then(Value::as_str)
    }

    pub fn compatibility_sanitizer(&self) -> Option<SanitizerKind> {
        match self.compatibility_str("sanitizer") {
            Some("gemini") => Some(SanitizerKind::Gemini),
            Some("moonshot") => Some(SanitizerKind::Moonshot),
            Some("none") => Some(SanitizerKind::None),
            _ => None,
        }
    }

    pub fn tool_updates(&self) -> Option<ToolUpdateEncoding> {
        self.route.protocol.tool_updates()
    }
}

fn missing_protocol(route: &RouteDescriptor, provider: &str) -> AiError {
    AiError {
        reason: AiErrorReason::UnsupportedOperation {
            message: format!("protocol not registered: {}", route.protocol),
            body: None,
            http: None,
            operation: "protocol".to_string(),
            provider: Some(provider.to_string()),
            route: Some(route.id.clone()),
        },
    }
}
