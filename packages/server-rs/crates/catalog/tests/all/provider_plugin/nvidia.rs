use std::collections::HashMap;

use opencode_contract::generated::ProviderID;

use crate::builtin::{Input, start};
use crate::provider_plugin::support::{headers, plugin_context, seed_provider};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nvidia_is_registered_so_legacy_referer_headers_can_be_applied() {
    assert!(
        opencode_catalog::provider_plugin::ORDER
            .iter()
            .any(|slot| slot.id() == opencode_catalog::provider_plugin::nvidia::ID)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn applies_nvidia_tracking_headers_only_to_nvidia() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _nvidia = seed_provider(
        &harness,
        "nvidia",
        "@opencode/ai/providers/openai-compatible",
        Some("https://integrate.api.nvidia.com/v1"),
        Some(vec![("Existing", "value")]),
    )
    .await;
    let _openrouter = seed_provider(&harness, "openrouter", "", None, None).await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["nvidia"], context).await;

    let nvidia = harness.services.provider.get(&ProviderID("nvidia".to_owned())).await.unwrap().unwrap();
    assert_eq!(
        headers(&nvidia),
        Some(HashMap::from([
            ("Existing".to_owned(), "value".to_owned()),
            ("HTTP-Referer".to_owned(), "https://opencode.ai/".to_owned()),
            ("X-Title".to_owned(), "opencode".to_owned()),
            ("X-BILLING-INVOKE-ORIGIN".to_owned(), "OpenCode".to_owned()),
        ]))
    );
    let openrouter = harness.services.provider.get(&ProviderID("openrouter".to_owned())).await.unwrap().unwrap();
    assert_eq!(headers(&openrouter), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adds_billing_origin_for_custom_nvidia_endpoints() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _nvidia = seed_provider(
        &harness,
        "nvidia",
        "@opencode/ai/providers/openai-compatible",
        Some("https://integrate.api.nvidia.com/v1"),
        None,
    )
    .await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["nvidia"], context).await;

    let nvidia = harness.services.provider.get(&ProviderID("nvidia".to_owned())).await.unwrap().unwrap();
    assert_eq!(
        headers(&nvidia),
        Some(HashMap::from([
            ("HTTP-Referer".to_owned(), "https://opencode.ai/".to_owned()),
            ("X-Title".to_owned(), "opencode".to_owned()),
            ("X-BILLING-INVOKE-ORIGIN".to_owned(), "OpenCode".to_owned()),
        ]))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preserves_an_explicit_nvidia_billing_origin_header() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _nvidia = seed_provider(
        &harness,
        "nvidia",
        "@opencode/ai/providers/openai-compatible",
        Some("https://integrate.api.nvidia.com/v1"),
        Some(vec![("X-BILLING-INVOKE-ORIGIN", "CustomOrigin")]),
    )
    .await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["nvidia"], context).await;

    let nvidia = harness.services.provider.get(&ProviderID("nvidia".to_owned())).await.unwrap().unwrap();
    assert_eq!(
        headers(&nvidia),
        Some(HashMap::from([
            ("HTTP-Referer".to_owned(), "https://opencode.ai/".to_owned()),
            ("X-Title".to_owned(), "opencode".to_owned()),
            ("X-BILLING-INVOKE-ORIGIN".to_owned(), "CustomOrigin".to_owned()),
        ]))
    );
}
