use std::collections::HashMap;

use opencode_contract::generated::ProviderID;

use crate::builtin::{Input, start};
use crate::provider_plugin::support::{headers, plugin_context, seed_provider};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zenmux_is_registered_so_legacy_referer_headers_can_be_applied() {
    assert!(
        opencode_catalog::provider_plugin::ORDER
            .iter()
            .any(|slot| slot.id() == opencode_catalog::provider_plugin::zenmux::ID)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn applies_the_exact_legacy_zenmux_headers() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _zenmux = seed_provider(
        &harness,
        "zenmux",
        "@opencode/ai/providers/openai-compatible",
        Some("https://zenmux.ai/api/v1"),
        None,
    )
    .await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["zenmux"], context).await;

    let zenmux = harness.services.provider.get(&ProviderID("zenmux".to_owned())).await.unwrap().unwrap();
    let zenmux_headers = headers(&zenmux).expect("the zenmux headers");
    assert_eq!(
        zenmux_headers,
        HashMap::from([
            ("HTTP-Referer".to_owned(), "https://opencode.ai/".to_owned()),
            ("X-Title".to_owned(), "opencode".to_owned()),
        ])
    );
    let mut names = zenmux_headers.keys().cloned().collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, vec!["HTTP-Referer".to_owned(), "X-Title".to_owned()]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn merges_legacy_zenmux_headers_with_existing_headers() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _zenmux = seed_provider(
        &harness,
        "zenmux",
        "@opencode/ai/providers/openai-compatible",
        Some("https://zenmux.ai/api/v1"),
        Some(vec![("Existing", "value")]),
    )
    .await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["zenmux"], context).await;

    let zenmux = harness.services.provider.get(&ProviderID("zenmux".to_owned())).await.unwrap().unwrap();
    assert_eq!(
        headers(&zenmux),
        Some(HashMap::from([
            ("Existing".to_owned(), "value".to_owned()),
            ("HTTP-Referer".to_owned(), "https://opencode.ai/".to_owned()),
            ("X-Title".to_owned(), "opencode".to_owned()),
        ]))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lets_configured_zenmux_legacy_headers_override_defaults() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _zenmux = seed_provider(
        &harness,
        "zenmux",
        "@opencode/ai/providers/openai-compatible",
        Some("https://zenmux.ai/api/v1"),
        Some(vec![("HTTP-Referer", "https://example.com/"), ("X-Title", "custom-title")]),
    )
    .await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["zenmux"], context).await;

    let zenmux = harness.services.provider.get(&ProviderID("zenmux".to_owned())).await.unwrap().unwrap();
    assert_eq!(
        headers(&zenmux),
        Some(HashMap::from([
            ("HTTP-Referer".to_owned(), "https://example.com/".to_owned()),
            ("X-Title".to_owned(), "custom-title".to_owned()),
        ]))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn guards_legacy_zenmux_headers_to_the_exact_zenmux_provider_id() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _openrouter = seed_provider(
        &harness,
        "openrouter",
        "",
        None,
        Some(vec![("HTTP-Referer", "https://example.com/"), ("X-Title", "custom-title")]),
    )
    .await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["zenmux"], context).await;

    let openrouter = harness.services.provider.get(&ProviderID("openrouter".to_owned())).await.unwrap().unwrap();
    assert_eq!(
        headers(&openrouter),
        Some(HashMap::from([
            ("HTTP-Referer".to_owned(), "https://example.com/".to_owned()),
            ("X-Title".to_owned(), "custom-title".to_owned()),
        ]))
    );
}
