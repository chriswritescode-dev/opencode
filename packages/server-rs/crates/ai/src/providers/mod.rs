use std::collections::BTreeMap;

use crate::Env;
use crate::route::model::Model;
use crate::schema::{AiError, AiErrorReason};

pub mod alibaba;
pub mod amazon_bedrock;
pub mod amazon_bedrock_mantle;
pub mod anthropic;
pub mod anthropic_compatible;
pub mod azure;
pub mod cerebras;
pub mod cloudflare_ai_gateway;
pub mod cloudflare_shared;
pub mod cloudflare_workers_ai;
pub mod cohere;
pub mod deepinfra;
pub mod digitalocean;
pub mod google;
pub mod google_adc;
pub mod google_vertex;
pub mod google_vertex_chat;
pub mod google_vertex_interactions;
pub mod google_vertex_messages;
pub mod google_vertex_mistral;
pub mod google_vertex_responses;
pub mod google_vertex_shared;
pub mod groq;
pub mod meta;
pub mod minimax;
pub mod mistral;
pub mod moonshot;
pub mod open_responses_options;
pub mod openai;
pub mod openai_compatible;
pub mod openai_compatible_responses;
pub mod openai_options;
pub mod openrouter;
pub mod settings;
pub mod togetherai;
pub mod venice;
pub mod vercel_ai_gateway;
pub mod xai;
pub mod zai;
pub mod zai_coding_plan;

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
            ("@opencode/ai/providers/alibaba", alibaba::model as Entry),
            ("@opencode/ai/providers/alibaba/chat", alibaba::model as Entry),
            ("@opencode/ai/providers/alibaba/messages", alibaba::messages_model as Entry),
            ("@opencode/ai/providers/alibaba/responses", alibaba::responses_model as Entry),
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
            ("@opencode/ai/providers/cloudflare-ai-gateway", cloudflare_ai_gateway::model as Entry),
            ("@opencode/ai/providers/cloudflare-workers-ai", cloudflare_workers_ai::model as Entry),
            ("@opencode/ai/providers/cohere", cohere::model as Entry),
            ("@opencode/ai/providers/cohere/chat", cohere::chat_model as Entry),
            ("@opencode/ai/providers/deepinfra", deepinfra::model as Entry),
            ("@opencode/ai/providers/digitalocean", digitalocean::model as Entry),
            ("@opencode/ai/providers/digitalocean/responses", digitalocean::responses_model as Entry),
            ("@opencode/ai/providers/google", google::model as Entry),
            ("@opencode/ai/providers/google/interactions", google::interactions_model as Entry),
            ("@opencode/ai/providers/google-vertex", google_vertex::model as Entry),
            ("@opencode/ai/providers/google-vertex-messages", google_vertex_messages::model as Entry),
            ("@opencode/ai/providers/google-vertex/chat", google_vertex_chat::model as Entry),
            ("@opencode/ai/providers/google-vertex/gemini", google_vertex::model as Entry),
            ("@opencode/ai/providers/google-vertex/interactions", google_vertex_interactions::model as Entry),
            ("@opencode/ai/providers/google-vertex/messages", google_vertex_messages::model as Entry),
            ("@opencode/ai/providers/google-vertex/mistral", google_vertex_mistral::model as Entry),
            ("@opencode/ai/providers/google-vertex/responses", google_vertex_responses::model as Entry),
            ("@opencode/ai/providers/groq", groq::model as Entry),
            ("@opencode/ai/providers/meta", meta::model as Entry),
            ("@opencode/ai/providers/meta/chat", meta::chat_model as Entry),
            ("@opencode/ai/providers/meta/messages", meta::messages_model as Entry),
            ("@opencode/ai/providers/meta/responses", meta::model as Entry),
            ("@opencode/ai/providers/minimax", minimax::model as Entry),
            ("@opencode/ai/providers/minimax/messages", minimax::model as Entry),
            ("@opencode/ai/providers/minimax/chat", minimax::chat_model as Entry),
            ("@opencode/ai/providers/minimax/responses", minimax::responses_model as Entry),
            ("@opencode/ai/providers/mistral", mistral::model as Entry),
            ("@opencode/ai/providers/moonshot", moonshot::model as Entry),
            ("@opencode/ai/providers/moonshot/chat", moonshot::model as Entry),
            ("@opencode/ai/providers/moonshot/messages", moonshot::messages_model as Entry),
            ("@opencode/ai/providers/moonshot/responses", moonshot::responses_model as Entry),
            ("@opencode/ai/providers/openai", openai::model as Entry),
            ("@opencode/ai/providers/openai/chat", openai::chat_model as Entry),
            ("@opencode/ai/providers/openai/responses", openai::model as Entry),
            ("@opencode/ai/providers/openai-compatible", openai_compatible::model as Entry),
            ("@opencode/ai/providers/openai-compatible-responses", openai_compatible_responses::model as Entry),
            ("@opencode/ai/providers/openrouter", openrouter::model as Entry),
            ("@opencode/ai/providers/openrouter/responses", openrouter::responses_model as Entry),
            ("@opencode/ai/providers/togetherai", togetherai::model as Entry),
            ("@opencode/ai/providers/venice", venice::model as Entry),
            ("@opencode/ai/providers/venice/responses", venice::responses_model as Entry),
            ("@opencode/ai/providers/vercel-ai-gateway", vercel_ai_gateway::model as Entry),
            ("@opencode/ai/providers/vercel-ai-gateway/chat", vercel_ai_gateway::chat_model as Entry),
            ("@opencode/ai/providers/vercel-ai-gateway/messages", vercel_ai_gateway::messages_model as Entry),
            ("@opencode/ai/providers/vercel-ai-gateway/responses", vercel_ai_gateway::responses_model as Entry),
            ("@opencode/ai/providers/xai", xai::model as Entry),
            ("@opencode/ai/providers/zai", zai::model as Entry),
            ("@opencode/ai/providers/zai/chat", zai::chat_model as Entry),
            ("@opencode/ai/providers/zai-coding-plan", zai_coding_plan::model as Entry),
            ("@opencode/ai/providers/zai-coding-plan/chat", zai_coding_plan::model as Entry),
            ("@opencode/ai/providers/zai-coding-plan/messages", zai_coding_plan::messages_model as Entry),
            ("@opencode/ai/providers/zai-coding-plan/responses", zai_coding_plan::responses_model as Entry),
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
