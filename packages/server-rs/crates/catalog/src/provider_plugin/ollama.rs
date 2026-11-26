use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use serde::{Deserialize, Serialize};

use crate::builtin::{Builtin, Services};
use crate::provider_plugin::BoxFuture;
use crate::provider_plugin::PluginContext;
use crate::provider_plugin::local::{self, Client, DiscoverError, Local, LocalModel};

pub const ID: &str = "opencode.provider.ollama";

const PROVIDER_ID: &str = "ollama";

static LOCAL: LazyLock<Local<Model>> =
    LazyLock::new(|| Local::new(PROVIDER_ID, "Ollama", "http://127.0.0.1:11434", &["/v1", "/api"], discover, model));

static SHOWS: LazyLock<Mutex<HashMap<String, ShowCache>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

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

#[derive(Clone, Default)]
struct ShowCache {
    api_key: Option<String>,
    entries: HashMap<String, ShowEntry>,
}

#[derive(Clone)]
struct ShowEntry {
    digest: String,
    info: ShowResponse,
}

#[derive(Clone, PartialEq, Deserialize)]
struct Details {
    #[serde(default)]
    parent_model: Option<String>,
    format: String,
    family: String,
    #[serde(default)]
    families: Option<Vec<String>>,
    parameter_size: String,
    quantization_level: String,
}

#[derive(Clone, PartialEq, Deserialize)]
struct Summary {
    name: String,
    model: String,
    #[serde(default)]
    remote_model: Option<String>,
    #[serde(default)]
    remote_host: Option<String>,
    modified_at: String,
    size: i64,
    digest: String,
    details: Details,
}

#[derive(Deserialize)]
struct TagsResponse {
    models: Vec<Summary>,
}

#[derive(Serialize)]
struct ShowRequest {
    model: String,
}

#[derive(Clone, PartialEq, Deserialize)]
struct ShowResponse {
    #[serde(default)]
    parameters: Option<String>,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    modified_at: Option<String>,
    #[serde(default)]
    details: Option<Details>,
    #[serde(default)]
    template: Option<String>,
    #[serde(default)]
    capabilities: Option<Vec<String>>,
    #[serde(default)]
    model_info: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Clone, PartialEq)]
struct Model {
    summary: Summary,
    show: ShowResponse,
}

fn discover(client: Client) -> BoxFuture<Result<Vec<Model>, DiscoverError>> {
    Box::pin(async move {
        let root = client.root().to_owned();
        let api_key = client.api_key().map(str::to_owned);
        let previous = {
            let mut shows = SHOWS.lock().expect("lock the ollama show cache");
            let previous = shows
                .get(&root)
                .filter(|cache| cache.api_key == api_key)
                .map(|cache| cache.entries.clone())
                .unwrap_or_default();
            shows.insert(root.clone(), ShowCache { api_key: api_key.clone(), entries: previous.clone() });
            previous
        };
        let response: TagsResponse = client.get_json("/api/tags").await?;
        let mut summaries: Vec<Summary> = response.models.into_iter().filter(|model| !model.model.is_empty()).collect();
        summaries.sort_by(|left, right| left.model.cmp(&right.model));

        let mut entries = HashMap::new();
        let mut models = Vec::new();
        for summary in summaries {
            let info = match previous.get(&summary.model).filter(|entry| entry.digest == summary.digest) {
                Some(entry) => Some(entry.info.clone()),
                None => client.post_json("/api/show", &ShowRequest { model: summary.model.clone() }).await.ok(),
            };
            let Some(info) = info else {
                continue;
            };
            entries.insert(summary.model.clone(), ShowEntry { digest: summary.digest.clone(), info: info.clone() });
            models.push(Model { summary, show: info });
        }
        SHOWS.lock().expect("lock the ollama show cache").insert(root, ShowCache { api_key, entries });

        Ok(models.into_iter().filter(|model| capability(&model.show, "completion")).collect())
    })
}

fn model(item: &Model) -> LocalModel {
    let family = item
        .show
        .details
        .as_ref()
        .map(|details| details.family.as_str())
        .filter(|family| !family.is_empty())
        .unwrap_or(&item.summary.details.family);
    LocalModel {
        id: item.summary.model.clone(),
        name: (!item.summary.name.is_empty()).then(|| item.summary.name.clone()),
        family: Some(family.to_owned()),
        tools: Some(capability(&item.show, "tools")),
        vision: Some(capability(&item.show, "vision")),
        context: item.show.model_info.as_ref().and_then(|info| {
            info.iter().find_map(|(key, value)| {
                if !key.ends_with(".context_length") {
                    return None;
                }
                let number = value.as_f64().filter(|number| *number > 0.0)?;
                Some(number as i64)
            })
        }),
        decision: false,
    }
}

fn capability(show: &ShowResponse, wanted: &str) -> bool {
    show.capabilities.as_ref().is_some_and(|capabilities| capabilities.iter().any(|item| item == wanted))
}
