use std::sync::Arc;

use serde_json::{Map, Value};

use crate::protocols::open_responses;
use crate::route::auth::Auth;
use crate::route::endpoint::Endpoint;
use crate::route::model::{Route, RouteTransport};
use crate::schema::RequestOptions;

pub const ID: &str = "openai-compatible-responses";

pub fn route() -> Route {
    let mut provider_options = Map::new();
    provider_options.insert("store".to_string(), Value::Bool(false));
    provider_options
        .insert("include".to_string(), Value::Array(vec![Value::String("reasoning.encrypted_content".to_string())]));
    Route {
        id: ID.to_string(),
        protocol: Arc::new(open_responses::OpenResponses),
        provider_metadata_key: Some("openresponses".to_string()),
        endpoint: Endpoint::path(open_responses::PATH),
        auth: Auth::None,
        headers: None,
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options: Some(RequestOptions { provider_options: Some(provider_options), ..Default::default() }),
    }
}
