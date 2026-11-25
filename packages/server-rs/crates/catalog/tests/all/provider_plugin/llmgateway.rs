use std::collections::HashMap;

use opencode_contract::generated::{IntegrationID, ProviderID, ProviderInfoActivation};

use crate::builtin::{Input, start};
use crate::provider_plugin::support::{headers, plugin_context, seed_provider};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn llmgateway_is_registered_so_legacy_referer_headers_can_be_applied() {
    assert!(
        opencode_catalog::provider_plugin::ORDER
            .iter()
            .any(|slot| slot.id() == opencode_catalog::provider_plugin::llmgateway::ID)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn applies_legacy_referer_headers_only_to_enabled_llmgateway() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _integrations = harness
        .services
        .integration
        .transform(|editor| {
            for id in ["llmgateway", "openrouter"] {
                editor.update(&IntegrationID(id.to_owned()), |_| {});
            }
            Ok(())
        })
        .await;
    let _gateway = seed_provider(
        &harness,
        "llmgateway",
        "@opencode/ai/providers/openai-compatible",
        Some("https://api.llmgateway.io/v1"),
        Some(vec![("Existing", "value")]),
    )
    .await;
    let _openrouter = seed_provider(&harness, "openrouter", "", None, None).await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["llmgateway"], context).await;
    let gateway = harness.services.provider.get(&ProviderID("llmgateway".to_owned())).await.unwrap().unwrap();
    let openrouter = harness.services.provider.get(&ProviderID("openrouter".to_owned())).await.unwrap().unwrap();
    assert_eq!(
        headers(&gateway),
        Some(HashMap::from([
            ("Existing".to_owned(), "value".to_owned()),
            ("HTTP-Referer".to_owned(), "https://opencode.ai/".to_owned()),
            ("X-Title".to_owned(), "opencode".to_owned()),
            ("X-Source".to_owned(), "opencode".to_owned()),
        ]))
    );
    assert_eq!(headers(&openrouter), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn does_not_apply_legacy_headers_to_a_disabled_llmgateway_provider() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _integrations = harness
        .services
        .integration
        .transform(|editor| {
            editor.update(&IntegrationID("llmgateway".to_owned()), |_| {});
            Ok(())
        })
        .await;
    let _gateway = seed_provider(
        &harness,
        "llmgateway",
        "@opencode/ai/providers/openai-compatible",
        Some("https://api.llmgateway.io/v1"),
        None,
    )
    .await;
    let _disabled = harness
        .services
        .provider
        .transform(|editor| {
            editor.update(&ProviderID("llmgateway".to_owned()), |provider| {
                provider.activation = ProviderInfoActivation::Disabled;
            });
            Ok(())
        })
        .await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["llmgateway"], context).await;
    let gateway = harness.services.provider.get(&ProviderID("llmgateway".to_owned())).await.unwrap().unwrap();
    assert_eq!(gateway.activation, ProviderInfoActivation::Disabled);
    assert_eq!(headers(&gateway), None);
}
