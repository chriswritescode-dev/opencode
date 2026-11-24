use std::sync::Arc;

use serde_json::{Map, Value};

use crate::Env;
use crate::protocols::anthropic_messages;
use crate::protocols::open_responses;
use crate::protocols::openai_chat;
use crate::protocols::shared::invalid_request;
use crate::protocols::utils::bedrock_auth;
use crate::providers::open_responses_options;
use crate::providers::openai_options;
use crate::providers::{RuntimeModel, Settings, language_model_with_route};
use crate::route::auth::Auth;
use crate::route::endpoint::{Endpoint, PathPart};
use crate::route::model::{Route, RouteTransport};
use crate::schema::{AiError, GenerationOptions, RequestOptions, StringMap, merge_request_options};

pub const ID: &str = "amazon-bedrock";
const SERVICE: &str = "bedrock-mantle";
const NAME: &str = "Bedrock Mantle";

#[derive(Clone, Copy)]
enum Kind {
    Responses,
    Chat,
    Messages,
}

pub fn model(model_id: &str, settings: &Settings, env: &dyn Env) -> Result<RuntimeModel, AiError> {
    build(Kind::Responses, model_id, settings, env)
}

pub fn responses_model(model_id: &str, settings: &Settings, env: &dyn Env) -> Result<RuntimeModel, AiError> {
    build(Kind::Responses, model_id, settings, env)
}

pub fn chat_model(model_id: &str, settings: &Settings, env: &dyn Env) -> Result<RuntimeModel, AiError> {
    build(Kind::Chat, model_id, settings, env)
}

pub fn messages_model(model_id: &str, settings: &Settings, env: &dyn Env) -> Result<RuntimeModel, AiError> {
    build(Kind::Messages, model_id, settings, env)
}

fn build(kind: Kind, model_id: &str, settings: &Settings, env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let api_key = settings.string("apiKey").map(str::to_string);
    let mode = match settings.string("auth") {
        Some("bearer") => Some(bedrock_auth::AuthMode::Bearer),
        Some("sigv4") => Some(bedrock_auth::AuthMode::Sigv4),
        _ => None,
    };
    if matches!(mode, Some(bedrock_auth::AuthMode::Bearer))
        && api_key.is_none()
        && env.var("AWS_BEARER_TOKEN_BEDROCK").is_none()
    {
        return Err(invalid_request("Amazon Bedrock Mantle bearer auth requires apiKey"));
    }
    if matches!(mode, Some(bedrock_auth::AuthMode::Sigv4)) && api_key.is_some() {
        return Err(invalid_request("Amazon Bedrock Mantle SigV4 auth does not accept apiKey"));
    }
    let credentials = bedrock_auth::credentials(settings.value("credentials"));
    let region = bedrock_auth::resolve_region(settings.string("region"), credentials.as_ref(), env);
    let auth = bedrock_auth::resolve_auth(
        bedrock_auth::ResolveAuthInput { api_key, credentials, profile: settings.optional_string("profile") },
        &region,
        bedrock_auth::ResolveAuthOptions { service: Some(SERVICE.to_string()), name: Some(NAME.to_string()), mode },
        env,
    )?;
    let base_url = settings.string("baseURL").map(str::to_string).unwrap_or_else(|| match kind {
        Kind::Messages => format!("https://bedrock-mantle.{region}.api.aws/anthropic/v1"),
        Kind::Responses | Kind::Chat => format!("https://bedrock-mantle.{region}.api.aws/v1"),
    });
    let settings_options =
        settings.request_options(&["apiKey", "auth", "baseURL", "credentials", "profile", "region", "topP"]);
    let top_p = settings.optional_number("topP").map(|top_p| RequestOptions {
        generation: GenerationOptions { top_p: Some(top_p), ..GenerationOptions::default() },
        ..RequestOptions::default()
    });
    let route = match kind {
        Kind::Responses => {
            let base = RequestOptions {
                provider_options: Some(open_responses_options::defaults()),
                ..RequestOptions::default()
            };
            let model_defaults = provider_options(openai_options::open_ai_default_options(model_id));
            route(
                "bedrock-mantle-responses",
                Arc::new(open_responses::OpenResponses),
                open_responses::PATH,
                base_url,
                auth,
                merge_request_options(&[
                    Some(&base),
                    model_defaults.as_ref(),
                    settings_options.as_ref(),
                    top_p.as_ref(),
                ]),
            )
        }
        Kind::Chat => {
            let model_defaults = provider_options(openai_options::open_ai_default_options(model_id));
            route(
                "bedrock-mantle-chat",
                Arc::new(openai_chat::OpenAiChat),
                openai_chat::PATH,
                base_url,
                auth,
                merge_request_options(&[model_defaults.as_ref(), settings_options.as_ref(), top_p.as_ref()]),
            )
        }
        Kind::Messages => route(
            "bedrock-mantle-messages",
            Arc::new(anthropic_messages::AnthropicMessages { effort_updates_require_5_1: true }),
            anthropic_messages::PATH,
            base_url,
            auth,
            merge_request_options(&[settings_options.as_ref(), top_p.as_ref()]),
        ),
    };
    Ok(language_model_with_route(model_id, ID, route, None, None))
}

fn route(
    id: &str,
    protocol: Arc<dyn crate::route::protocol::Protocol>,
    path: &str,
    base_url: String,
    auth: Auth,
    options: Option<RequestOptions>,
) -> Route {
    Route {
        id: id.to_string(),
        protocol,
        provider_metadata_key: Some("mantle".to_string()),
        endpoint: Endpoint {
            base_url: Some(base_url),
            path: PathPart::Static(path.to_string()),
            query: StringMap::new(),
        },
        auth,
        headers: None,
        body: None,
        channel: None,
        transport: RouteTransport::Http,
        options,
    }
}

fn provider_options(options: Option<Map<String, Value>>) -> Option<RequestOptions> {
    options.map(|provider_options| RequestOptions {
        provider_options: Some(provider_options),
        ..RequestOptions::default()
    })
}
