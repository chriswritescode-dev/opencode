use std::sync::Arc;

use crate::Env;
use crate::protocols::google_interactions::GoogleInteractions;
use crate::providers::google_vertex_shared::{self, OAuthInput};
use crate::providers::{RuntimeModel, Settings, language_model_with_route};
use crate::route::auth::Auth;
use crate::route::endpoint::{Endpoint, PathPart};
use crate::route::model::{Route, RouteTransport};
use crate::schema::{AiError, StringMap};

pub const ID: &str = google_vertex_shared::ID;

const ROUTE_ID: &str = "google-vertex-interactions";
const PATH: &str = "/interactions";

pub fn model(model_id: &str, settings: &Settings, env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let api_key = google_vertex_shared::api_key(&google_vertex_shared::settings_api_key_input(settings), env)?;
    let location = google_vertex_shared::location(settings.string("location"), env);
    let project = google_vertex_shared::project(settings.string("project"), env);
    let base_url = match settings.string("baseURL") {
        Some(base_url) => base_url.to_string(),
        None => match &api_key {
            Some(_) => {
                format!("https://{}/v1beta1/locations/{location}", google_vertex_shared::host(&location))
            }
            None => format!(
                "https://{}/v1beta1/projects/{}/locations/{location}",
                google_vertex_shared::host(&location),
                google_vertex_shared::require_project(project.as_deref())?
            ),
        },
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
        protocol: Arc::new(GoogleInteractions),
        provider_metadata_key: Some(google_vertex_shared::METADATA_KEY.to_string()),
        endpoint: Endpoint {
            base_url: Some(base_url),
            path: PathPart::Static(PATH.to_string()),
            query: StringMap::from_pairs([("alt".to_string(), "sse".to_string())]),
        },
        auth,
        headers: Some(Arc::new(google_vertex_shared::request_type_header)),
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options: settings.request_options(&google_vertex_shared::CONNECTION_KEYS),
    };
    Ok(language_model_with_route(model_id, ID, route, None, None))
}
