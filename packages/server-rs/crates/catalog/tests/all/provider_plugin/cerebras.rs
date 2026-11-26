use std::collections::HashMap;

use opencode_contract::codec::JsonMap;
use opencode_contract::generated::ProviderID;

use crate::builtin::{Harness, Input, start};
use crate::provider_plugin::support::plugin_context;

async fn register_cerebras(
    harness: &mut Harness,
    id: &str,
    package: &str,
    headers: Option<Vec<(&str, &str)>>,
) -> Option<HashMap<String, String>> {
    let provider = ProviderID(id.to_owned());
    let package = package.to_owned();
    let headers = headers
        .map(|headers| JsonMap(headers.into_iter().map(|(name, value)| (name.to_owned(), value.to_owned())).collect()));
    let _seeded = harness
        .services
        .provider
        .transform({
            let provider = provider.clone();
            move |editor| {
                editor.update(&provider, |provider| {
                    provider.package = package.clone();
                    provider.headers = headers.clone();
                });
                Ok(())
            }
        })
        .await;
    let context = plugin_context(harness);
    harness.register_provider_plugins(&["cerebras"], context).await;
    harness
        .services
        .provider
        .get(&provider)
        .await
        .unwrap()
        .unwrap()
        .headers
        .map(|headers| headers.0.into_iter().collect())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cerebras_applies_the_legacy_integration_header() {
    let mut harness = start(Input::new(Vec::new())).await;
    let headers =
        register_cerebras(&mut harness, "cerebras", "@opencode/ai/providers/cerebras", Some(vec![("Existing", "1")]))
            .await
            .expect("the headers");
    assert_eq!(
        headers,
        HashMap::from([
            ("Existing".to_owned(), "1".to_owned()),
            ("X-Cerebras-3rd-Party-Integration".to_owned(), "opencode".to_owned()),
        ])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cerebras_ignores_non_cerebras_providers() {
    let mut harness = start(Input::new(Vec::new())).await;
    let headers = register_cerebras(&mut harness, "groq", "aisdk:@ai-sdk/openai-compatible", None).await;
    assert_eq!(headers, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cerebras_applies_the_integration_header_to_custom_native_providers() {
    let mut harness = start(Input::new(Vec::new())).await;
    let headers = register_cerebras(
        &mut harness,
        "custom-cerebras",
        "@opencode/ai/providers/cerebras",
        Some(vec![("Existing", "1")]),
    )
    .await
    .expect("the headers");
    assert_eq!(
        headers,
        HashMap::from([
            ("Existing".to_owned(), "1".to_owned()),
            ("X-Cerebras-3rd-Party-Integration".to_owned(), "opencode".to_owned()),
        ])
    );
}
