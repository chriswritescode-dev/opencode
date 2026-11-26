use opencode_contract::generated::ProviderID;

use crate::builtin::{Input, start};
use crate::provider_plugin::support::{base_url, plugin_context, seed_provider};

async fn base_url_for(host: &str) -> Option<String> {
    let mut harness = start(Input::new(Vec::new()).environment("DATABRICKS_HOST", host)).await;
    let provider_id = ProviderID("databricks".to_owned());
    let _seeded =
        seed_provider(&harness, "databricks", "", Some("https://${DATABRICKS_HOST}/ai-gateway/mlflow/v1"), None).await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["databricks"], context).await;
    let provider = harness.services.provider.get(&provider_id).await.unwrap().unwrap();
    base_url(&provider)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fills_the_base_url_from_a_bare_host() {
    assert_eq!(
        base_url_for("adb-123.azuredatabricks.net").await,
        Some("https://adb-123.azuredatabricks.net/ai-gateway/mlflow/v1".to_owned())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fills_the_base_url_from_a_host_that_includes_the_scheme() {
    assert_eq!(
        base_url_for("https://adb-123.azuredatabricks.net/").await,
        Some("https://adb-123.azuredatabricks.net/ai-gateway/mlflow/v1".to_owned())
    );
}
