use std::sync::LazyLock;

use serde::Deserialize;

use crate::builtin::{Builtin, Services};
use crate::provider_plugin::BoxFuture;
use crate::provider_plugin::PluginContext;
use crate::provider_plugin::local::{self, Client, DiscoverError, Local, LocalModel};

pub const ID: &str = "opencode.provider.vllm";

const PROVIDER_ID: &str = "vllm";

static LOCAL: LazyLock<Local<RemoteModel>> =
    LazyLock::new(|| Local::new(PROVIDER_ID, "vLLM", "http://127.0.0.1:8000", &["/v1"], discover, model));

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
struct RemoteModel {
    id: String,
    owned_by: String,
    max_model_len: Option<i64>,
}

#[derive(Deserialize)]
struct Response {
    data: Vec<RemoteModel>,
}

fn discover(client: Client) -> BoxFuture<Result<Vec<RemoteModel>, DiscoverError>> {
    Box::pin(async move {
        client.get("/health").await?;
        let response: Response = client.get_json("/v1/models").await?;
        let mut models: Vec<RemoteModel> =
            response.data.into_iter().filter(|model| model.owned_by == PROVIDER_ID && !model.id.is_empty()).collect();
        models.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(models)
    })
}

fn model(item: &RemoteModel) -> LocalModel {
    LocalModel {
        id: item.id.clone(),
        name: None,
        family: None,
        tools: None,
        vision: None,
        context: item.max_model_len,
        decision: false,
    }
}
