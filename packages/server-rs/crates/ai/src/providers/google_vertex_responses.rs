use std::sync::Arc;

use crate::Env;
use crate::protocols::open_responses::{self, OpenResponses};
use crate::providers::{
    RuntimeModel, Settings, google_vertex_shared, language_model_with_route, open_responses_options,
};
use crate::route::auth::Auth;
use crate::route::endpoint::{Endpoint, PathPart};
use crate::route::model::{Route, RouteTransport};
use crate::schema::{AiError, RequestOptions, StringMap};

pub const ID: &str = google_vertex_shared::ID;

const ROUTE_ID: &str = "google-vertex-responses";

pub fn model(model_id: &str, settings: &Settings, env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let options =
        RequestOptions { provider_options: Some(open_responses_options::defaults()), ..RequestOptions::default() };
    let route = Route {
        id: ROUTE_ID.to_string(),
        protocol: Arc::new(OpenResponses),
        provider_metadata_key: Some(google_vertex_shared::METADATA_KEY.to_string()),
        endpoint: Endpoint {
            base_url: None,
            path: PathPart::Static(open_responses::PATH.to_string()),
            query: StringMap::new(),
        },
        auth: Auth::None,
        headers: None,
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options: Some(options),
    };
    let input = google_vertex_shared::oauth_settings("Responses", settings)?;
    let route = google_vertex_shared::oauth_route(
        route,
        "Responses",
        input,
        |project, location| {
            format!("https://aiplatform.googleapis.com/v1/projects/{project}/locations/{location}/endpoints/openapi")
        },
        env,
    )?;
    Ok(language_model_with_route(model_id, ID, route, None, None))
}
