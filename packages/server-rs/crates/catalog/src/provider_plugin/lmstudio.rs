use std::sync::LazyLock;

use serde::Deserialize;

use crate::builtin::{Builtin, Services};
use crate::provider_plugin::BoxFuture;
use crate::provider_plugin::PluginContext;
use crate::provider_plugin::local::{self, Client, DiscoverError, Local, LocalModel};

pub const ID: &str = "opencode.provider.lmstudio";

const PROVIDER_ID: &str = "lmstudio";

const MODELS_PATH: &str = "/api/v1/models";

static LOCAL: LazyLock<Local<Model>> = LazyLock::new(|| {
    Local::new(PROVIDER_ID, "LM Studio", "http://127.0.0.1:1234", &["/api/v1", "/v1"], discover, model)
});

pub async fn register(services: &Services, context: &PluginContext, builtin: &mut Builtin) {
    register_with(services, context, builtin, local::Options::default()).await
}

pub async fn register_with(
    services: &Services,
    context: &PluginContext,
    builtin: &mut Builtin,
    options: local::Options,
) {
    LOCAL.register(services, context, builtin, options).await
}

#[derive(Clone, PartialEq, Deserialize)]
struct Response {
    models: Vec<Model>,
}

#[derive(Clone, PartialEq, Deserialize)]
struct Model {
    r#type: String,
    key: String,
    display_name: String,
    #[serde(default)]
    architecture: Option<String>,
    loaded_instances: Vec<LoadedInstance>,
    max_context_length: Option<i64>,
    #[serde(default)]
    capabilities: Option<Capabilities>,
    #[serde(default)]
    has_vision_adapter: Option<bool>,
}

#[derive(Clone, PartialEq, Deserialize)]
struct LoadedInstance {
    config: LoadedConfig,
}

#[derive(Clone, PartialEq, Deserialize)]
struct LoadedConfig {
    context_length: i64,
}

#[derive(Clone, PartialEq, Deserialize)]
struct Capabilities {
    vision: bool,
    trained_for_tool_use: bool,
}

fn discover(client: Client) -> BoxFuture<Result<Vec<Model>, DiscoverError>> {
    Box::pin(async move {
        let response: Response = client.get_json(MODELS_PATH).await?;
        let mut models: Vec<Model> = response
            .models
            .into_iter()
            .filter(|model| (model.r#type == "llm" || model.r#type == "decision") && !model.key.is_empty())
            .collect();
        models.sort_by(|left, right| left.key.cmp(&right.key));
        Ok(models)
    })
}

fn model(item: &Model) -> LocalModel {
    LocalModel {
        id: item.key.clone(),
        name: (!item.display_name.is_empty()).then(|| item.display_name.clone()),
        family: item.architecture.clone(),
        tools: item.capabilities.as_ref().map(|capabilities| capabilities.trained_for_tool_use),
        vision: match &item.capabilities {
            Some(capabilities) => Some(capabilities.vision),
            None => item.has_vision_adapter,
        },
        context: if item.loaded_instances.is_empty() {
            item.max_context_length
        } else {
            item.loaded_instances.iter().map(|instance| instance.config.context_length).min()
        },
        decision: item.r#type == "decision",
    }
}
