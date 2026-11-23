use std::sync::Arc;

use serde_json::Value;

use crate::Env;
use crate::ExecutionContext;
use crate::protocols::mistral_chat::{self, MistralChat};
use crate::providers::google_vertex_shared;
use crate::providers::{RuntimeModel, Settings, language_model_with_route};
use crate::route::auth::Auth;
use crate::route::endpoint::{Endpoint, PathPart};
use crate::route::framing::Definition;
use crate::route::model::{Model, ResolvedRequest, Route, RouteTransport};
use crate::route::protocol::{Protocol, ProtocolState};
use crate::schema::{AiError, StringMap};

pub const ID: &str = google_vertex_shared::ID;

const ROUTE_ID: &str = "google-vertex-mistral";

pub struct VertexMistralChat;

impl Protocol for VertexMistralChat {
    fn id(&self) -> &str {
        mistral_chat::ID
    }

    fn build_body(
        &self,
        request: &ResolvedRequest,
        model: &Model,
        _context: &ExecutionContext,
    ) -> Result<Value, AiError> {
        let mut body = mistral_chat::from_request(&request.request, model)?;
        if let Some(object) = body.as_object_mut() {
            object.remove("prompt_cache_key");
        }
        Ok(body)
    }

    fn default_path(&self, model: &Model, _body: &Value) -> Option<String> {
        Some(format!("/{}:streamRawPredict", model.id))
    }

    fn framing(&self) -> Definition {
        MistralChat.framing()
    }

    fn new_state(&self, request: &ResolvedRequest, context: &ExecutionContext) -> Box<dyn ProtocolState> {
        MistralChat.new_state(request, context)
    }
}

pub fn model(model_id: &str, settings: &Settings, env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let route = Route {
        id: ROUTE_ID.to_string(),
        protocol: Arc::new(VertexMistralChat),
        provider_metadata_key: Some(google_vertex_shared::METADATA_KEY.to_string()),
        endpoint: Endpoint { base_url: None, path: PathPart::Default, query: StringMap::new() },
        auth: Auth::None,
        headers: None,
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options: None,
    };
    let input = google_vertex_shared::oauth_settings("Mistral", settings)?;
    let route = google_vertex_shared::oauth_route(
        route,
        "Mistral",
        input,
        |project, location| {
            format!(
                "https://{}/v1/projects/{project}/locations/{location}/publishers/mistralai/models",
                google_vertex_shared::host(location)
            )
        },
        env,
    )?;
    Ok(language_model_with_route(model_id, ID, route, None, None))
}
