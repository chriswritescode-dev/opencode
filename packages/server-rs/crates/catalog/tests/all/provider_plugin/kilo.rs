use std::collections::HashMap;

use opencode_contract::generated::ProviderID;

use crate::builtin::{Input, start};
use crate::provider_plugin::support::{headers, plugin_context, seed_provider};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kilo_is_registered_so_legacy_referer_headers_can_be_applied() {
    assert!(
        opencode_catalog::provider_plugin::ORDER
            .iter()
            .any(|slot| slot.id() == opencode_catalog::provider_plugin::kilo::ID)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn applies_legacy_referer_headers_only_to_kilo_endpoints() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _kilo = seed_provider(
        &harness,
        "kilo",
        "@opencode/ai/providers/openai-compatible",
        Some("https://api.kilo.ai/api/gateway"),
        Some(vec![("Existing", "value")]),
    )
    .await;
    let _openrouter = seed_provider(&harness, "openrouter", "", None, None).await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["kilo"], context).await;
    let kilo = harness.services.provider.get(&ProviderID("kilo".to_owned())).await.unwrap().unwrap();
    let openrouter = harness.services.provider.get(&ProviderID("openrouter".to_owned())).await.unwrap().unwrap();
    assert_eq!(
        headers(&kilo),
        Some(HashMap::from([
            ("Existing".to_owned(), "value".to_owned()),
            ("HTTP-Referer".to_owned(), "https://opencode.ai/".to_owned()),
            ("X-Title".to_owned(), "opencode".to_owned()),
        ]))
    );
    assert_eq!(headers(&openrouter), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uses_the_exact_legacy_kilo_header_casing_and_set() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _kilo = seed_provider(
        &harness,
        "kilo",
        "@opencode/ai/providers/openai-compatible",
        Some("https://api.kilo.ai/api/gateway"),
        None,
    )
    .await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["kilo"], context).await;
    let kilo = harness.services.provider.get(&ProviderID("kilo".to_owned())).await.unwrap().unwrap();
    let kilo_headers = headers(&kilo).expect("the legacy kilo headers");
    assert_eq!(
        kilo_headers,
        HashMap::from([
            ("HTTP-Referer".to_owned(), "https://opencode.ai/".to_owned()),
            ("X-Title".to_owned(), "opencode".to_owned()),
        ])
    );
    assert!(!kilo_headers.contains_key("http-referer"));
    assert!(!kilo_headers.contains_key("x-title"));
    assert!(!kilo_headers.contains_key("X-Source"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uses_endpoint_package_matching_instead_of_a_provider_id_guard() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _kilo = seed_provider(&harness, "kilo", "aisdk:kilo", None, None).await;
    let _custom = seed_provider(
        &harness,
        "custom-kilo",
        "@opencode/ai/providers/openai-compatible",
        Some("https://api.kilo.ai/api/gateway"),
        None,
    )
    .await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["kilo"], context).await;
    let kilo = harness.services.provider.get(&ProviderID("kilo".to_owned())).await.unwrap().unwrap();
    let custom = harness.services.provider.get(&ProviderID("custom-kilo".to_owned())).await.unwrap().unwrap();
    assert_eq!(headers(&kilo), None);
    assert_eq!(
        headers(&custom),
        Some(HashMap::from([
            ("HTTP-Referer".to_owned(), "https://opencode.ai/".to_owned()),
            ("X-Title".to_owned(), "opencode".to_owned()),
        ]))
    );
}
