use std::collections::HashMap;

use opencode_catalog::provider_plugin::{ORDER, openrouter};
use opencode_contract::generated::ProviderID;

use crate::builtin::{Input, start};
use crate::provider_plugin::support::{headers, plugin_context, seed_provider};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn openrouter_is_registered_so_legacy_openrouter_behavior_can_be_applied() {
    assert!(ORDER.iter().any(|slot| slot.id() == openrouter::ID));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn applies_legacy_referer_headers_only_to_openrouter() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _openrouter = seed_provider(
        &harness,
        "openrouter",
        "@opencode/ai/providers/openrouter",
        None,
        Some(vec![("Existing", "value")]),
    )
    .await;
    let _nvidia = seed_provider(&harness, "nvidia", "", None, None).await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["openrouter"], context).await;

    let openrouter = harness.services.provider.get(&ProviderID("openrouter".to_owned())).await.unwrap().unwrap();
    assert_eq!(
        headers(&openrouter),
        Some(HashMap::from([
            ("Existing".to_owned(), "value".to_owned()),
            ("HTTP-Referer".to_owned(), "https://opencode.ai/".to_owned()),
            ("X-Title".to_owned(), "opencode".to_owned()),
        ]))
    );

    let nvidia = harness.services.provider.get(&ProviderID("nvidia".to_owned())).await.unwrap().unwrap();
    assert_eq!(headers(&nvidia), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn filters_openrouter_gpt_5_chat_alias() {
    let mut harness = start(Input::new(Vec::new())).await;
    let openrouter = ProviderID("openrouter".to_owned());
    let openai = ProviderID("openai".to_owned());
    let _openrouter = seed_provider(&harness, "openrouter", "@opencode/ai/providers/openrouter", None, None).await;
    let _openai = harness
        .services
        .provider
        .transform({
            let openai = openai.clone();
            move |editor| {
                editor.update(&openai, |_| {});
                Ok(())
            }
        })
        .await;
    let _models = harness
        .services
        .model
        .transform({
            let openrouter = openrouter.clone();
            let openai = openai.clone();
            move |editor| {
                editor.update(&openrouter, "openai/gpt-5-chat", |_| {});
                editor.update(&openrouter, "openai/gpt-5", |_| {});
                editor.update(&openai, "openai/gpt-5-chat", |_| {});
                Ok(())
            }
        })
        .await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["openrouter"], context).await;

    let filtered = harness.services.model.get(&openrouter, "openai/gpt-5-chat").await.unwrap().unwrap();
    assert!(!filtered.enabled);
    let kept = harness.services.model.get(&openrouter, "openai/gpt-5").await.unwrap().unwrap();
    assert!(kept.enabled);
    let unrelated = harness.services.model.get(&openai, "openai/gpt-5-chat").await.unwrap().unwrap();
    assert!(unrelated.enabled);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn does_not_disable_gpt_5_chat_latest_for_non_openrouter_providers() {
    let mut harness = start(Input::new(Vec::new())).await;
    let custom = ProviderID("custom-openrouter".to_owned());
    let _provider = harness
        .services
        .provider
        .transform({
            let custom = custom.clone();
            move |editor| {
                editor.update(&custom, |_| {});
                Ok(())
            }
        })
        .await;
    let _models = harness
        .services
        .model
        .transform({
            let custom = custom.clone();
            move |editor| {
                editor.update(&custom, "gpt-5-chat-latest", |_| {});
                Ok(())
            }
        })
        .await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["openrouter"], context).await;

    let model = harness.services.model.get(&custom, "gpt-5-chat-latest").await.unwrap().unwrap();
    assert!(model.enabled);
}
