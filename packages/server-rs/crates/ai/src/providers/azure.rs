use std::sync::Arc;

use serde_json::{Map, Value, json};
use url::Url;

use crate::Env;
use crate::media::route::MediaEndpoint;
use crate::modalities::image::ImageModel;
use crate::protocols::openai_images::{ImageInput, OpenAiImages, pixel_size};
use crate::protocols::shared::invalid_request;
use crate::protocols::{anthropic_messages, open_responses_channel, openai_chat, openai_embeddings, openai_responses};
use crate::providers::open_responses_options;
use crate::providers::openai_options;
use crate::providers::{RuntimeModel, Settings, language_model_with_route, modality_options};
use crate::route::auth::{Auth, ProviderAuthOptions, api_key};
use crate::route::embedding::{EmbeddingRoute, EmbeddingRouteInput};
use crate::route::endpoint::{Endpoint, PathPart, trim_base_url};
use crate::route::model::{Route, RouteTransport};
use crate::schema::{AiError, RequestOptions, StringMap, merge_request_options};

pub const ID: &str = "azure";

const RESPONSES_ID: &str = "azure-openai-responses";
const CHAT_ID: &str = "azure-openai-chat";
const MESSAGES_ID: &str = "azure-anthropic-messages";
const RESPONSES_WEBSOCKET_ROTATE_AFTER_MS: u64 = 55 * 60 * 1000;
const DECLARED: [&str; 6] =
    ["apiKey", "apiVersion", "resourceName", "useDeploymentBasedUrls", "baseURL", "queryParams"];

pub fn model(model_id: &str, settings: &Settings, env: &dyn Env) -> Result<RuntimeModel, AiError> {
    responses_model(model_id, settings, env)
}

pub fn responses_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let override_defaults = with_provider_options(open_responses_options::defaults());
    let model_defaults = openai_options::open_ai_default_options(model_id).map(with_provider_options);
    let settings_options = settings.request_options(&DECLARED);
    let options =
        merge_request_options(&[Some(&override_defaults), model_defaults.as_ref(), settings_options.as_ref()]);
    Ok(language_model_with_route(
        model_id,
        ID,
        Route {
            id: RESPONSES_ID.to_string(),
            protocol: Arc::new(openai_responses::OpenAiResponses),
            provider_metadata_key: Some(ID.to_string()),
            endpoint: openai_endpoint(settings, model_id, openai_responses::PATH)?,
            auth: auth(settings, "api-key"),
            headers: None,
            body: None,
            channel: Some(Arc::new(channel_options())),
            transport: RouteTransport::Http,
            options,
        },
        None,
        None,
    ))
}

pub fn chat_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let model_defaults = openai_options::open_ai_default_options(model_id).map(with_provider_options);
    let settings_options = settings.request_options(&DECLARED);
    let options = merge_request_options(&[model_defaults.as_ref(), settings_options.as_ref()]);
    Ok(language_model_with_route(
        model_id,
        ID,
        Route {
            id: CHAT_ID.to_string(),
            protocol: Arc::new(openai_chat::OpenAiChat),
            provider_metadata_key: Some(ID.to_string()),
            endpoint: openai_endpoint(settings, model_id, openai_chat::PATH)?,
            auth: auth(settings, "api-key"),
            headers: None,
            body: None,
            channel: None,
            transport: RouteTransport::Http,
            options,
        },
        None,
        Some(json!({ "promptCacheKey": true })),
    ))
}

pub fn messages_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    Ok(language_model_with_route(
        model_id,
        ID,
        Route {
            id: MESSAGES_ID.to_string(),
            protocol: Arc::new(anthropic_messages::AnthropicMessages::default()),
            provider_metadata_key: Some(ID.to_string()),
            endpoint: messages_endpoint(settings)?,
            auth: auth(settings, "x-api-key"),
            headers: None,
            body: None,
            channel: None,
            transport: RouteTransport::Http,
            options: settings.request_options(&DECLARED),
        },
        None,
        None,
    ))
}

pub fn embedding_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = modality_options(settings, &DECLARED);
    let endpoint = openai_endpoint(settings, model_id, openai_embeddings::PATH)?;
    let endpoint = MediaEndpoint {
        base_url: endpoint.base_url,
        path: Arc::new(|_| openai_embeddings::PATH.to_string()),
        query: endpoint.query,
    };
    let route = EmbeddingRoute::make(EmbeddingRouteInput {
        id: openai_embeddings::ID.to_string(),
        provider: ID.to_string(),
        protocol: Arc::new(openai_embeddings::OpenAiEmbeddings),
        endpoint,
        auth: auth(settings, "api-key"),
        headers: None,
        provider_metadata_key: None,
        options: transport,
        max_values: None,
    });
    Ok(RuntimeModel::Embedding(route.model(model_id, provider_options)))
}

pub fn image_model(model_id: &str, settings: &Settings, _env: &dyn Env) -> Result<RuntimeModel, AiError> {
    let (transport, provider_options) = modality_options(settings, &DECLARED);
    let (base_url, query) = openai_target(settings, model_id)?;
    let endpoint = MediaEndpoint {
        base_url: Some(base_url),
        path: Arc::new(|request: &crate::modalities::image::ImageRequest| {
            if request.images.is_empty() {
                crate::protocols::openai_images::PATH.to_string()
            } else {
                crate::protocols::openai_images::EDIT_PATH.to_string()
            }
        }),
        query,
    };
    Ok(RuntimeModel::Image(ImageModel::streamed(
        model_id,
        ID.to_string(),
        Arc::new(OpenAiImages::new(ID, Arc::new(pixel_size), ImageInput::Edits)),
        endpoint,
        auth(settings, "api-key"),
        None,
        transport,
        provider_options,
    )))
}

fn auth(settings: &Settings, header: &str) -> Auth {
    Auth::remove("authorization").and_then(api_key(
        ProviderAuthOptions { auth: None, api_key: settings.api_key() },
        &["AZURE_OPENAI_API_KEY"],
        |credential| credential.header(header),
    ))
}

fn openai_endpoint(settings: &Settings, model_id: &str, path: &str) -> Result<Endpoint, AiError> {
    let (base_url, query) = openai_target(settings, model_id)?;
    Ok(Endpoint { base_url: Some(base_url), path: PathPart::Static(path.to_string()), query })
}

fn openai_target(settings: &Settings, model_id: &str) -> Result<(String, StringMap), AiError> {
    let configured_base = settings.string("baseURL");
    let base = match configured_base {
        Some(base) => trim_base_url(base).to_string(),
        None => match settings.string("resourceName") {
            Some(resource) => format!("https://{}.openai.azure.com/openai", resource.trim()),
            None => return Err(requires_url()),
        },
    };
    let mut query = StringMap::new();
    query.insert("api-version".to_string(), settings.string("apiVersion").unwrap_or("v1").to_string());
    if let Some(params) = settings.query_params() {
        for (key, value) in params.iter() {
            query.insert(key.to_string(), value.to_string());
        }
    }
    let (base_url, query) = if settings.boolean("useDeploymentBasedUrls").unwrap_or(false) {
        (format!("{base}/deployments/{model_id}"), query)
    } else if configured_base.is_some_and(|base| !is_azure_host(base)) {
        (base, settings.query_params().unwrap_or_default())
    } else {
        (format!("{base}/v1"), query)
    };
    Ok((base_url, query))
}

fn messages_endpoint(settings: &Settings) -> Result<Endpoint, AiError> {
    let base_url = match settings.string("baseURL") {
        Some(base) => trim_base_url(base).to_string(),
        None => match settings.string("resourceName") {
            Some(resource) => format!("https://{}.services.ai.azure.com/anthropic/v1", resource.trim()),
            None => return Err(requires_url()),
        },
    };
    Ok(Endpoint {
        base_url: Some(base_url),
        path: PathPart::Default,
        query: settings.query_params().unwrap_or_default(),
    })
}

fn is_azure_host(value: &str) -> bool {
    Url::parse(value)
        .ok()
        .and_then(|url| url.host_str().map(|host| host.ends_with(".openai.azure.com")))
        .unwrap_or(false)
}

fn requires_url() -> AiError {
    invalid_request("Azure requires resourceName or baseURL")
}

fn with_provider_options(options: Map<String, Value>) -> RequestOptions {
    RequestOptions { provider_options: Some(options), ..RequestOptions::default() }
}

fn channel_options() -> open_responses_channel::Options {
    open_responses_channel::Options {
        id: RESPONSES_ID.to_string(),
        name: "Azure OpenAI Responses".to_string(),
        rotate_after_ms: Some(RESPONSES_WEBSOCKET_ROTATE_AFTER_MS),
        enabled: Some(Arc::new(channel_enabled)),
        url: Some(Arc::new(channel_url)),
        headers: None,
        continuation: None,
    }
}

fn channel_enabled(value: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    url.scheme() == "https"
        && url.host_str().is_some_and(|host| host.ends_with(".openai.azure.com"))
        && url.path().ends_with("/openai/v1/responses")
        && url.query_pairs().any(|(key, value)| key.as_ref() == "api-version" && value.as_ref() == "v1")
}

fn channel_url(value: &str) -> String {
    let Ok(mut url) = Url::parse(value) else {
        return value.to_string();
    };
    let pairs = url
        .query_pairs()
        .filter(|(key, _)| key.as_ref() != "api-version")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    url.set_query(None);
    if !pairs.is_empty() {
        let mut query = url.query_pairs_mut();
        for (key, value) in pairs {
            query.append_pair(&key, &value);
        }
    }
    url.to_string()
}
