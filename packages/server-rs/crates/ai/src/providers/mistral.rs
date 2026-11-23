use std::sync::Arc;

use crate::Env;
use crate::protocols::mistral_chat::{self, MistralChat};
use crate::providers::{RuntimeModel, Settings, language_model_with_route};
use crate::route::auth::{ProviderAuthOptions, api_key};
use crate::route::endpoint::Endpoint;
use crate::route::model::{Route, RouteTransport};
use crate::schema::AiError;

pub const ID: &str = "mistral";

pub fn model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let base_url = settings.string("baseURL").unwrap_or(mistral_chat::DEFAULT_BASE_URL);
    let auth =
        api_key(ProviderAuthOptions { auth: None, api_key: settings.api_key() }, &["MISTRAL_API_KEY"], |credential| {
            credential.bearer()
        });
    let route = Route {
        id: mistral_chat::ID.to_string(),
        protocol: Arc::new(MistralChat),
        provider_metadata_key: Some("mistral".to_string()),
        endpoint: Endpoint::new(base_url, mistral_chat::PATH),
        auth,
        headers: None,
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options: settings.request_options(&["apiKey", "baseURL"]),
    };
    Ok(language_model_with_route(model_id, ID, route, None, None))
}
