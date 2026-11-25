use opencode_contract::codec::encode_json;
use opencode_contract::generated::ProviderID;
use serde_json::json;

use crate::builtin::{Input, catalog_model, json_value, seed, start};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_provider_keeps_provider_and_model_compatibility_precedence_across_config_documents() {
    let mut harness = start(Input::new(vec![
        json!({
            "providers": {
                "proxy": {
                    "compatibility": { "requireFinishReason": false, "supportsEffortUpdates": false },
                    "models": { "claude": { "compatibility": { "supportsPromptCacheKey": true } } }
                }
            }
        }),
        json!({
            "providers": {
                "proxy": {
                    "compatibility": { "supportsPromptCacheKey": false },
                    "models": { "alias": { "modelID": "catalog" } }
                }
            }
        }),
    ]))
    .await;
    let _seeded = seed(
        &harness,
        "proxy",
        "@opencode/ai/providers/openai/chat",
        vec![catalog_model("catalog", "proxy", "@opencode/ai/providers/openai/chat")],
    )
    .await;
    harness.register(&["config_provider"]).await;

    let provider_id = ProviderID("proxy".to_owned());
    let provider = harness.services.provider.get(&provider_id).await.unwrap().unwrap();
    assert_eq!(
        json_value(&encode_json(&provider.compatibility.expect("the provider compatibility"))),
        json!({ "requireFinishReason": false, "supportsPromptCacheKey": false })
    );

    let claude = harness.services.model.get(&provider_id, "claude").await.unwrap().unwrap();
    assert_eq!(
        json_value(&encode_json(&claude.compatibility.expect("the claude compatibility"))),
        json!({ "requireFinishReason": false, "supportsPromptCacheKey": true })
    );

    let alias = harness.services.model.get(&provider_id, "alias").await.unwrap().unwrap();
    assert_eq!(
        json_value(&encode_json(&alias.compatibility.expect("the alias compatibility"))),
        json!({ "requireFinishReason": false, "supportsPromptCacheKey": false })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_provider_applies_provider_compatibility_to_every_provider_model_with_model_overrides() {
    let mut harness = start(Input::new(vec![json!({
        "providers": {
            "proxy": {
                "compatibility": { "supportsPromptCacheKey": false, "requireFinishReason": false },
                "models": { "configured": { "compatibility": { "supportsPromptCacheKey": true } } }
            }
        }
    })]))
    .await;
    let mut catalog = catalog_model("catalog", "proxy", "@opencode/ai/providers/openai/chat");
    catalog["compatibility"] = json!({ "maxTokensField": "max_tokens" });
    let _seeded = seed(&harness, "proxy", "@opencode/ai/providers/openai/chat", vec![catalog]).await;
    harness.register(&["config_provider"]).await;

    let provider_id = ProviderID("proxy".to_owned());
    let catalog = harness.services.model.get(&provider_id, "catalog").await.unwrap().unwrap();
    assert_eq!(
        json_value(&encode_json(&catalog.compatibility.expect("the catalog compatibility"))),
        json!({
            "maxTokensField": "max_tokens",
            "supportsPromptCacheKey": false,
            "requireFinishReason": false
        })
    );

    let configured = harness.services.model.get(&provider_id, "configured").await.unwrap().unwrap();
    assert_eq!(
        json_value(&encode_json(&configured.compatibility.expect("the configured compatibility"))),
        json!({ "supportsPromptCacheKey": true, "requireFinishReason": false })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_provider_preserves_catalog_capabilities_unless_config_overrides_them() {
    let mut harness = start(Input::new(vec![json!({
        "providers": {
            "custom": {
                "package": "aisdk:@ai-sdk/openai-compatible",
                "models": {
                    "inherited": { "name": "Inherited" },
                    "overridden": {
                        "capabilities": { "tools": true, "input": ["text", "image"], "output": ["text"] }
                    }
                }
            }
        }
    })]))
    .await;
    let mut inherited = catalog_model("inherited", "custom", "aisdk:@ai-sdk/openai-compatible");
    inherited["capabilities"] = json!({ "tools": false, "input": ["text"], "output": ["text"] });
    let mut overridden = catalog_model("overridden", "custom", "aisdk:@ai-sdk/openai-compatible");
    overridden["capabilities"] = json!({ "tools": false, "input": ["text"], "output": ["text"] });
    let _seeded = seed(&harness, "custom", "aisdk:@ai-sdk/openai-compatible", vec![inherited, overridden]).await;
    harness.register(&["config_provider"]).await;

    let provider_id = ProviderID("custom".to_owned());
    let inherited = harness.services.model.get(&provider_id, "inherited").await.unwrap().unwrap();
    assert_eq!(
        json_value(&encode_json(&inherited.capabilities)),
        json!({ "tools": false, "input": ["text"], "output": ["text"] })
    );

    let overridden = harness.services.model.get(&provider_id, "overridden").await.unwrap().unwrap();
    assert_eq!(
        json_value(&encode_json(&overridden.capabilities)),
        json!({ "tools": true, "input": ["text", "image"], "output": ["text"] })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_provider_resolves_a_config_model_with_only_input_output_capabilities_using_default_tools() {
    let mut harness = start(Input::new(vec![json!({
        "providers": {
            "custom": {
                "package": "aisdk:@ai-sdk/openai-compatible",
                "models": {
                    "demo": {
                        "name": "Demo",
                        "capabilities": { "input": ["text"], "output": ["text"] }
                    }
                }
            }
        }
    })]))
    .await;
    harness.register(&["config_provider"]).await;

    let demo = harness
        .services
        .model
        .get(&ProviderID("custom".to_owned()), "demo")
        .await
        .unwrap()
        .expect("the configured demo model");
    assert_eq!(
        json_value(&encode_json(&demo.capabilities)),
        json!({ "tools": true, "input": ["text"], "output": ["text"] })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_provider_preserves_existing_catalog_metadata_when_migrated_fields_are_omitted() {
    let mut harness = start(Input::new(vec![json!({
        "provider": { "custom": { "models": { "chat": {} } } }
    })]))
    .await;
    let mut chat = catalog_model("chat", "custom", "aisdk:@ai-sdk/anthropic");
    chat["settings"] = json!({ "baseURL": "https://catalog.example/v1" });
    chat["capabilities"] = json!({ "tools": false, "input": ["audio"], "output": ["audio"] });
    chat["limit"] = json!({ "context": 100000, "input": 80000, "output": 16000 });
    chat["cost"] = json!([{ "input": 1, "output": 2, "cache": { "read": 0, "write": 0 } }]);
    chat["variants"] = json!([{ "id": "high", "settings": { "effort": "high" } }]);
    let _seeded = seed(&harness, "custom", "aisdk:@ai-sdk/anthropic", vec![chat]).await;
    let provider_id = ProviderID("custom".to_owned());
    let _rewritten = harness
        .services
        .provider
        .transform({
            let provider_id = provider_id.clone();
            move |editor| {
                editor.models_update(&provider_id, "chat", |_| {});
                Ok(())
            }
        })
        .await;
    let before = harness.services.model.get(&provider_id, "chat").await.unwrap().unwrap();

    harness.register(&["config_provider"]).await;

    let after = harness.services.model.get(&provider_id, "chat").await.unwrap().unwrap();
    assert_eq!(json_value(&encode_json(&after)), json_value(&encode_json(&before)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_provider_keeps_catalog_capabilities_that_a_migrated_override_omits() {
    let mut harness = start(Input::new(vec![json!({
        "provider": { "custom": { "models": { "chat": { "tool_call": true } } } }
    })]))
    .await;
    let mut chat = catalog_model("chat", "custom", "aisdk:@ai-sdk/openai-compatible");
    chat["capabilities"] = json!({ "tools": false, "input": ["audio"], "output": ["audio"] });
    let _seeded = seed(&harness, "custom", "aisdk:@ai-sdk/openai-compatible", vec![chat]).await;
    harness.register(&["config_provider"]).await;

    let chat = harness
        .services
        .model
        .get(&ProviderID("custom".to_owned()), "chat")
        .await
        .unwrap()
        .expect("the configured chat model");
    assert_eq!(
        json_value(&encode_json(&chat.capabilities)),
        json!({ "tools": true, "input": ["audio"], "output": ["audio"] })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_provider_applies_configured_release_date_temperature_support_and_reasoning_field() {
    let mut harness = start(Input::new(vec![json!({
        "providers": {
            "custom": {
                "package": "aisdk:@ai-sdk/openai-compatible",
                "models": {
                    "reasoner": {
                        "releaseDate": "2026-02-03",
                        "capabilities": { "temperature": false, "freeform": ["text", "regex"] },
                        "compatibility": { "reasoningField": "reasoning_content" }
                    }
                }
            }
        }
    })]))
    .await;
    harness.register(&["config_provider"]).await;

    let reasoner = harness
        .services
        .model
        .get(&ProviderID("custom".to_owned()), "reasoner")
        .await
        .unwrap()
        .expect("the configured reasoner model");
    assert_eq!(reasoner.time.released, 1770076800000.0);
    assert_eq!(reasoner.capabilities.temperature, Some(false));
    assert_eq!(json_value(&encode_json(&reasoner.capabilities.freeform)), json!(["text", "regex"]));
    assert_eq!(
        json_value(&encode_json(&reasoner.compatibility.expect("the reasoner compatibility").reasoning_field.unwrap())),
        json!("reasoning_content")
    );
}
