use std::sync::Arc;

use crate::Env;
use crate::protocols::openai_chat::{self, OpenAiChat};
use crate::providers::google_vertex_shared;
use crate::providers::{RuntimeModel, Settings, language_model_with_route};
use crate::route::auth::Auth;
use crate::route::endpoint::{Endpoint, PathPart};
use crate::route::model::{Route, RouteTransport};
use crate::schema::{AiError, StringMap};

pub const ID: &str = google_vertex_shared::ID;

const ROUTE_ID: &str = "google-vertex-chat";

pub fn model(model_id: &str, settings: &Settings, env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let route = Route {
        id: ROUTE_ID.to_string(),
        protocol: Arc::new(OpenAiChat),
        provider_metadata_key: Some(google_vertex_shared::METADATA_KEY.to_string()),
        endpoint: Endpoint {
            base_url: None,
            path: PathPart::Static(openai_chat::PATH.to_string()),
            query: StringMap::new(),
        },
        auth: Auth::None,
        headers: None,
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options: None,
    };
    let input = google_vertex_shared::oauth_settings("Chat", settings)?;
    let route = google_vertex_shared::oauth_route(
        route,
        "Chat",
        input,
        |project, location| {
            format!("https://aiplatform.googleapis.com/v1/projects/{project}/locations/{location}/endpoints/openapi")
        },
        env,
    )?;
    Ok(language_model_with_route(model_id, ID, route, None, None))
}
