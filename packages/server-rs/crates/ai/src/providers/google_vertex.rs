use std::sync::Arc;

use serde_json::{Map, Value};

use crate::Env;
use crate::ExecutionContext;
use crate::media::route::MediaEndpoint;
use crate::modalities::embedding::EmbeddingRequest;
use crate::protocols::gemini;
use crate::protocols::google_embed_content::GoogleVertexEmbedContent;
use crate::protocols::google_vertex_predict_embeddings::{self, GoogleVertexPredictEmbeddings};
use crate::protocols::shared::invalid_request;
use crate::providers::google_vertex_shared::{self, OAuthInput};
use crate::providers::{RuntimeModel, Settings, embedding_options, language_model_with_route};
use crate::route::auth::Auth;
use crate::route::embedding::{EmbeddingProtocol, EmbeddingRoute, EmbeddingRouteInput};
use crate::route::endpoint::{Endpoint, PathPart};
use crate::route::framing::Definition;
use crate::route::model::{Model, ResolvedRequest, Route, RouteTransport};
use crate::route::protocol::{Protocol, ProtocolState};
use crate::schema::{AiError, SanitizerKind, StringMap};

pub const ID: &str = google_vertex_shared::ID;

const ROUTE_ID: &str = "google-vertex-gemini";
const EXPRESS_BASE_URL: &str = "https://aiplatform.googleapis.com/v1/publishers/google";

pub struct VertexGemini;

impl VertexGemini {
    fn default_path(model: &Model) -> String {
        let prefix = if model.id.starts_with("endpoints/") { "" } else { "models/" };
        format!("/{prefix}{}:streamGenerateContent?alt=sse", model.id)
    }
}

impl Protocol for VertexGemini {
    fn id(&self) -> &str {
        gemini::ID
    }

    fn build_body(
        &self,
        request: &ResolvedRequest,
        model: &Model,
        context: &ExecutionContext,
    ) -> Result<Value, AiError> {
        let mut body = gemini::Gemini.build_body(request, model, context)?;
        if let Some(object) = body.as_object_mut() {
            object.remove("serviceTier");
        }
        let labels = labels(request.request.provider_options.as_ref());
        if let Some(labels) = labels
            && let Some(object) = body.as_object_mut()
        {
            object.insert("labels".to_string(), Value::Object(labels));
        }
        Ok(body)
    }

    fn default_path(&self, model: &Model, _body: &Value) -> Option<String> {
        Some(VertexGemini::default_path(model))
    }

    fn framing(&self) -> Definition {
        gemini::Gemini.framing()
    }

    fn new_state(&self, request: &ResolvedRequest, context: &ExecutionContext) -> Box<dyn ProtocolState> {
        gemini::Gemini.new_state(request, context)
    }

    fn sanitizer(&self) -> Option<SanitizerKind> {
        Some(SanitizerKind::Gemini)
    }
}

fn labels(options: Option<&Map<String, Value>>) -> Option<Map<String, Value>> {
    let labels = options?.get("labels")?.as_object()?;
    Some(
        labels
            .iter()
            .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), Value::String(value.to_string()))))
            .collect::<Map<_, _>>(),
    )
}

pub fn model(model_id: &str, settings: &Settings, env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let api_key = google_vertex_shared::api_key(&google_vertex_shared::settings_api_key_input(settings), env)?;
    let endpoint_model = model_id.starts_with("endpoints/");
    if api_key.is_some() && endpoint_model {
        return Err(invalid_request("Google Vertex tuned models do not support Express Mode API keys"));
    }
    let location = google_vertex_shared::location(settings.string("location"), env);
    let project = google_vertex_shared::project(settings.string("project"), env);
    let base_url = match settings.string("baseURL") {
        Some(base_url) => base_url.to_string(),
        None if api_key.is_some() => EXPRESS_BASE_URL.to_string(),
        None => format!(
            "https://{}/v1beta1/projects/{}/locations/{}{}",
            google_vertex_shared::host(&location),
            google_vertex_shared::require_project(project.as_deref())?,
            location,
            if endpoint_model { "" } else { "/publishers/google" }
        ),
    };
    let auth = match &api_key {
        Some(api_key) => Auth::headers([("x-goog-api-key".to_string(), api_key.clone())]),
        None => google_vertex_shared::oauth(
            &OAuthInput { access_token: settings.string("accessToken").map(str::to_string), auth: None },
            project.as_deref(),
        )?,
    };
    let route = Route {
        id: ROUTE_ID.to_string(),
        protocol: Arc::new(VertexGemini),
        provider_metadata_key: Some(google_vertex_shared::METADATA_KEY.to_string()),
        endpoint: Endpoint { base_url: Some(base_url), path: PathPart::Default, query: StringMap::new() },
        auth,
        headers: Some(Arc::new(google_vertex_shared::request_type_header)),
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options: settings.request_options(&google_vertex_shared::CONNECTION_KEYS),
    };
    Ok(language_model_with_route(model_id, ID, route, None, None))
}

pub fn embedding_model(model_id: &str, settings: &Settings, env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let api_key = google_vertex_shared::api_key(&google_vertex_shared::settings_api_key_input(settings), env)?;
    let location = google_vertex_shared::location(settings.string("location"), env);
    let project = google_vertex_shared::project(settings.string("project"), env);
    let configured_base = settings.string("baseURL");
    let base_url = match configured_base {
        Some(base_url) => base_url.to_string(),
        None if api_key.is_some() => EXPRESS_BASE_URL.to_string(),
        None => format!(
            "https://{}/v1beta1/projects/{}/locations/{}/publishers/google",
            google_vertex_shared::host(&location),
            google_vertex_shared::require_project(project.as_deref())?,
            location
        ),
    };
    let auth = match &api_key {
        Some(api_key) => Auth::headers([("x-goog-api-key".to_string(), api_key.clone())]),
        None => google_vertex_shared::oauth(
            &OAuthInput { access_token: settings.string("accessToken").map(str::to_string), auth: None },
            project.as_deref(),
        )?,
    };
    let (transport, provider_options) = embedding_options(settings, &google_vertex_shared::CONNECTION_KEYS);
    if model_id.starts_with("gemini-embedding-2") {
        if api_key.is_some() && configured_base.is_none() {
            return Err(invalid_request(
                "Google Vertex embedContent does not support Express Mode API keys; use project credentials",
            ));
        }
        let model = model_id.to_string();
        let endpoint =
            MediaEndpoint::function(Arc::new(move |_: &EmbeddingRequest| format!("/models/{model}:embedContent")))
                .with_base_url(base_url);
        let route = EmbeddingRoute::make(EmbeddingRouteInput {
            id: GoogleVertexEmbedContent.id().to_string(),
            provider: ID.to_string(),
            protocol: Arc::new(GoogleVertexEmbedContent),
            endpoint,
            auth,
            headers: None,
            provider_metadata_key: Some(google_vertex_shared::METADATA_KEY.to_string()),
            options: transport,
            max_values: Some(1),
        });
        return Ok(RuntimeModel::Embedding(route.model(model_id, provider_options)));
    }
    let path = if model_id.starts_with("endpoints/") {
        format!("/{model_id}:predict")
    } else {
        format!("/models/{model_id}:predict")
    };
    let max_values =
        if model_id.starts_with("gemini-embedding-001") { 1 } else { google_vertex_predict_embeddings::MAX_INSTANCES };
    let endpoint = MediaEndpoint::function(Arc::new(move |_: &EmbeddingRequest| path.clone())).with_base_url(base_url);
    let route = EmbeddingRoute::make(EmbeddingRouteInput {
        id: google_vertex_predict_embeddings::ID.to_string(),
        provider: ID.to_string(),
        protocol: Arc::new(GoogleVertexPredictEmbeddings),
        endpoint,
        auth,
        headers: None,
        provider_metadata_key: Some(google_vertex_shared::METADATA_KEY.to_string()),
        options: transport,
        max_values: Some(max_values),
    });
    Ok(RuntimeModel::Embedding(route.model(model_id, provider_options)))
}
