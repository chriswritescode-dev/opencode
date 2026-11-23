use std::collections::BTreeMap;

use crate::Env;
use crate::route::model::Model;
use crate::schema::{AiError, AiErrorReason};

pub mod amazon_bedrock;
pub mod amazon_bedrock_mantle;
pub mod anthropic;
pub mod anthropic_compatible;
pub mod azure;
pub mod cerebras;
pub mod deepinfra;
pub mod google;
pub mod open_responses_options;
pub mod openai;
pub mod openai_compatible;
pub mod openai_compatible_responses;
pub mod openai_options;
pub mod settings;
pub mod togetherai;
pub mod venice;

pub use settings::Settings;

#[derive(Clone)]
pub enum RuntimeModel {
    Language(Model),
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn language_model(
    model_id: &str,
    provider: &str,
    route_id: &str,
    provider_metadata_key: Option<&str>,
    protocol: std::sync::Arc<dyn crate::route::protocol::Protocol>,
    endpoint: crate::route::endpoint::Endpoint,
    auth: crate::route::auth::Auth,
    options: Option<crate::schema::RequestOptions>,
    compatibility: Option<serde_json::Value>,
    capabilities: Option<serde_json::Value>,
) -> RuntimeModel {
    language_model_with_route(
        model_id,
        provider,
        crate::route::model::Route {
            id: route_id.to_string(),
            protocol,
            provider_metadata_key: provider_metadata_key.map(str::to_string),
            endpoint,
            auth,
            headers: None,
            body: None,
            channel: None,
            transport: crate::route::model::RouteTransport::Http,
            options: options.clone(),
        },
        compatibility,
        capabilities,
    )
}

pub(crate) fn language_model_with_route(
    model_id: &str,
    provider: &str,
    route: crate::route::model::Route,
    compatibility: Option<serde_json::Value>,
    capabilities: Option<serde_json::Value>,
) -> RuntimeModel {
    let options = route.options.clone();
    RuntimeModel::Language(Model {
        id: model_id.to_string(),
        provider: provider.to_string(),
        route,
        options,
        compatibility,
        capabilities,
    })
}

pub type Entry = fn(&str, &Settings, &dyn Env) -> Result<RuntimeModel, AiError>;

pub struct ProviderPackages {
    entries: BTreeMap<&'static str, Entry>,
}

impl ProviderPackages {
    pub fn builtin() -> Self {
        let entries = BTreeMap::from([
            ("@opencode/ai/providers/amazon-bedrock", amazon_bedrock::model as Entry),
            ("@opencode/ai/providers/amazon-bedrock/mantle", amazon_bedrock_mantle::model as Entry),
            ("@opencode/ai/providers/amazon-bedrock/mantle/chat", amazon_bedrock_mantle::chat_model as Entry),
            ("@opencode/ai/providers/amazon-bedrock/mantle/messages", amazon_bedrock_mantle::messages_model as Entry),
            ("@opencode/ai/providers/amazon-bedrock/mantle/responses", amazon_bedrock_mantle::responses_model as Entry),
            ("@opencode/ai/providers/anthropic", anthropic::model as Entry),
            ("@opencode/ai/providers/anthropic-compatible", anthropic_compatible::model as Entry),
            ("@opencode/ai/providers/azure", azure::model as Entry),
            ("@opencode/ai/providers/azure/chat", azure::chat_model as Entry),
            ("@opencode/ai/providers/azure/messages", azure::messages_model as Entry),
            ("@opencode/ai/providers/azure/responses", azure::responses_model as Entry),
            ("@opencode/ai/providers/cerebras", cerebras::model as Entry),
            ("@opencode/ai/providers/deepinfra", deepinfra::model as Entry),
            ("@opencode/ai/providers/google", google::model as Entry),
            ("@opencode/ai/providers/openai", openai::model as Entry),
            ("@opencode/ai/providers/openai/chat", openai::chat_model as Entry),
            ("@opencode/ai/providers/openai/responses", openai::model as Entry),
            ("@opencode/ai/providers/openai-compatible", openai_compatible::model as Entry),
            ("@opencode/ai/providers/openai-compatible-responses", openai_compatible_responses::model as Entry),
            ("@opencode/ai/providers/togetherai", togetherai::model as Entry),
            ("@opencode/ai/providers/venice/responses", venice::responses_model as Entry),
        ]);
        Self { entries }
    }

    pub fn model(
        &self,
        specifier: &str,
        model_id: &str,
        settings: &Settings,
        env: &dyn Env,
    ) -> Result<RuntimeModel, AiError> {
        let entry = self.entries.get(specifier).ok_or_else(|| unknown_package(specifier))?;
        entry(model_id, settings, env)
    }
}

fn unknown_package(specifier: &str) -> AiError {
    AiError {
        reason: AiErrorReason::UnsupportedOperation {
            message: format!("unknown provider package: {specifier}"),
            body: None,
            http: None,
            operation: "provider-package".to_string(),
            provider: None,
            route: None,
        },
    }
}
