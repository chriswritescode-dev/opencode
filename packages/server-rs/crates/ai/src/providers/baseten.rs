use std::sync::Arc;

use serde_json::{Map, Value};

use crate::Env;
use crate::media::protocol::{Identity, TextResponse};
use crate::media::route::MediaEndpoint;
use crate::modalities::decision::{DecisionRequest, DecisionResponse};
use crate::protocols::openai_chat;
use crate::protocols::system_one::{self, SystemOne};
use crate::providers::{RuntimeModel, Settings, language_model, modality_options};
use crate::route::auth::{ProviderAuthOptions, api_key};
use crate::route::decision::{DecisionProtocol, DecisionRoute, DecisionRouteInput};
use crate::route::endpoint::Endpoint;
use crate::schema::AiError;

pub const ID: &str = "baseten";

const BASE_URL: &str = "https://inference.baseten.co/v1";
const CHAT_ID: &str = "baseten-chat";
const METADATA_KEY: &str = "baseten";
const DECLARED: [&str; 2] = ["apiKey", "baseURL"];

pub struct BasetenSystemOne;

impl DecisionProtocol for BasetenSystemOne {
    fn id(&self) -> &str {
        "baseten-system-one"
    }

    fn name(&self) -> &str {
        "System One"
    }

    fn inline_media(&self) -> bool {
        true
    }

    fn body_from(&self, request: &DecisionRequest) -> Result<Map<String, Value>, AiError> {
        let mut body = system_one::encode_body(request, "state.messages")?;
        body.insert("model".to_string(), Value::String(request.model.id.clone()));
        Ok(body)
    }

    fn decode(
        &self,
        identity: &Identity,
        text: &TextResponse,
        request: &DecisionRequest,
        provider_metadata_key: &str,
    ) -> Result<DecisionResponse, AiError> {
        SystemOne.decode(identity, text, request, provider_metadata_key)
    }
}

pub fn model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let auth =
        api_key(ProviderAuthOptions { auth: None, api_key: settings.api_key() }, &["BASETEN_API_KEY"], |credential| {
            credential.bearer()
        });
    Ok(language_model(
        model_id,
        ID,
        CHAT_ID,
        Some(METADATA_KEY),
        Arc::new(openai_chat::OpenAiChat),
        Endpoint::new(settings.string("baseURL").unwrap_or(BASE_URL), openai_chat::PATH),
        auth,
        settings.request_options(&DECLARED),
        None,
        None,
    ))
}

pub fn decision_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = modality_options(settings, &DECLARED);
    let auth =
        api_key(ProviderAuthOptions { auth: None, api_key: settings.api_key() }, &["BASETEN_API_KEY"], |credential| {
            credential.bearer()
        });
    let route = DecisionRoute::make(DecisionRouteInput {
        id: BasetenSystemOne.id().to_string(),
        provider: ID.to_string(),
        protocol: Arc::new(BasetenSystemOne),
        endpoint: MediaEndpoint::path(system_one::PATH).with_base_url(settings.string("baseURL").unwrap_or(BASE_URL)),
        auth,
        headers: None,
        provider_metadata_key: Some(METADATA_KEY.to_string()),
        options: transport,
    });
    Ok(RuntimeModel::Decision(route.model(model_id, None, provider_options)))
}
