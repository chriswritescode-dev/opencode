use std::sync::Arc;

use crate::Env;
use crate::media::route::MediaEndpoint;
use crate::protocols::system_one::{self, SystemOne};
use crate::providers::{RuntimeModel, Settings, modality_options};
use crate::route::auth::{ProviderAuthOptions, bearer};
use crate::route::decision::{DecisionRoute, DecisionRouteInput};
use crate::schema::{AiError, StringMap};

pub const ID: &str = "opencode";

const BASE_URL: &str = "https://opencode.ai/inference/systemone/v1";
const DECLARED: [&str; 2] = ["apiKey", "baseURL"];

pub fn decision_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = modality_options(settings, &DECLARED);
    let auth = bearer(ProviderAuthOptions { auth: None, api_key: settings.api_key() }, &["OPENCODE_API_KEY"]);
    let route = DecisionRoute::make(DecisionRouteInput {
        id: system_one::ID.to_string(),
        provider: ID.to_string(),
        protocol: Arc::new(SystemOne),
        endpoint: MediaEndpoint::path(system_one::PATH).with_base_url(settings.string("baseURL").unwrap_or(BASE_URL)),
        auth,
        headers: Some(danger_zone_headers()),
        provider_metadata_key: Some("opencode".to_string()),
        options: transport,
    });
    Ok(RuntimeModel::Decision(route.model(model_id, None, provider_options)))
}

fn danger_zone_headers() -> StringMap {
    StringMap::from_pairs([("TypeSafe-Danger-Zone-Images".to_string(), "true".to_string())])
}
