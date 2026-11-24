use std::sync::Arc;

use serde_json::Value;

use crate::Env;
use crate::ExecutionContext;
use crate::prepare::tool_updates::ToolUpdateEncoding;
use crate::protocols::anthropic_messages::{self, AnthropicMessages};
use crate::providers::google_vertex_shared;
use crate::providers::{RuntimeModel, Settings, language_model_with_route};
use crate::route::auth::Auth;
use crate::route::endpoint::{Endpoint, PathPart};
use crate::route::framing::Definition;
use crate::route::model::{Model, ResolvedRequest, Route, RouteTransport};
use crate::route::protocol::{Protocol, ProtocolState};
use crate::schema::{AiError, LlmRequest, StringMap};

pub const ID: &str = google_vertex_shared::ID;

const ROUTE_ID: &str = "google-vertex-messages";
const ANTHROPIC_VERSION: &str = "vertex-2023-10-16";
const ANTHROPIC_HEADER_VERSION: &str = "2023-06-01";

pub struct VertexAnthropicMessages;

impl Protocol for VertexAnthropicMessages {
    fn id(&self) -> &str {
        anthropic_messages::ID
    }

    fn build_body(
        &self,
        request: &ResolvedRequest,
        model: &Model,
        _context: &ExecutionContext,
    ) -> Result<Value, AiError> {
        let body = anthropic_messages::from_request(&request.request, model)?;
        let mut object = body.as_object().cloned().unwrap_or_default();
        object.shift_remove("model");
        object.insert("anthropic_version".to_string(), Value::String(ANTHROPIC_VERSION.to_string()));
        Ok(Value::Object(object))
    }

    fn default_path(&self, model: &Model, _body: &Value) -> Option<String> {
        Some(format!("/{}:streamRawPredict", model.id))
    }

    fn framing(&self) -> Definition {
        AnthropicMessages::default().framing()
    }

    fn body_headers(&self, model: &Model, request: &LlmRequest, body: &Value) -> Vec<(String, String)> {
        AnthropicMessages::default().body_headers(model, request, body)
    }

    fn new_state(&self, request: &ResolvedRequest, context: &ExecutionContext) -> Box<dyn ProtocolState> {
        AnthropicMessages::default().new_state(request, context)
    }

    fn supports_effort_updates(&self, model: &Model, request: &LlmRequest) -> bool {
        AnthropicMessages::default().supports_effort_updates(model, request)
    }

    fn tool_updates(&self) -> Option<ToolUpdateEncoding> {
        AnthropicMessages::default().tool_updates()
    }
}

fn anthropic_version(_request: &LlmRequest) -> Vec<(String, String)> {
    vec![("anthropic-version".to_string(), ANTHROPIC_HEADER_VERSION.to_string())]
}

pub fn model(model_id: &str, settings: &Settings, env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let route = Route {
        id: ROUTE_ID.to_string(),
        protocol: Arc::new(VertexAnthropicMessages),
        provider_metadata_key: Some(google_vertex_shared::METADATA_KEY.to_string()),
        endpoint: Endpoint { base_url: None, path: PathPart::Default, query: StringMap::new() },
        auth: Auth::None,
        headers: Some(Arc::new(anthropic_version)),
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options: None,
    };
    let input = google_vertex_shared::oauth_settings("Messages", settings)?;
    let route = google_vertex_shared::oauth_route(
        route,
        "Messages",
        input,
        |project, location| {
            format!(
                "https://{}/v1/projects/{project}/locations/{location}/publishers/anthropic/models",
                google_vertex_shared::host(location)
            )
        },
        env,
    )?;
    Ok(language_model_with_route(model_id, ID, route, None, None))
}
