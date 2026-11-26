use opencode_contract::codec::{decode_json, encode_json};
use opencode_contract::generated::{ModelVariant, ProviderID};
use serde_json::json;

use crate::builtin::{Input, catalog_model, json_value, seed, start};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_provider_generates_variants_after_rewriting_a_configured_model_package() {
    let mut harness = start(Input::new(vec![json!({
        "providers": {
            "custom": {
                "package": "aisdk:@ai-sdk/openai",
                "models": {
                    "claude": { "modelID": "claude-opus-4-8", "package": "aisdk:@ai-sdk/anthropic" }
                }
            }
        }
    })]))
    .await;
    harness.register(&["config_provider"]).await;

    let model = harness.services.model.get(&ProviderID("custom".to_owned()), "claude").await.unwrap().unwrap();
    assert_eq!(model.package.as_deref(), Some("@opencode/ai/providers/anthropic"));
    assert_eq!(
        model.variants.iter().map(|variant| variant.id.as_str()).collect::<Vec<_>>(),
        ["low", "medium", "high", "xhigh", "max"]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_provider_preserves_plugin_supplied_variants_on_custom_models_when_config_omits_variants() {
    let variants: Vec<ModelVariant> = decode_json::<Vec<ModelVariant>>(
        r#"[{"id":"none","settings":{"reasoningEffort":"none"}},{"id":"low","settings":{"reasoningEffort":"low"}},{"id":"medium","settings":{"reasoningEffort":"medium"}},{"id":"xhigh","settings":{"reasoningEffort":"xhigh"}}]"#,
    )
    .expect("the plugin variants");

    let mut harness = start(Input::new(vec![json!({
        "providers": {
            "custom": {
                "package": "aisdk:@ai-sdk/openai-compatible",
                "models": {
                    "plain": { "name": "Plain" },
                    "same-package": { "package": "aisdk:@ai-sdk/openai-compatible" }
                }
            },
            "per-model": {
                "models": {
                    "only-model-package": { "package": "aisdk:@ai-sdk/openai-compatible" }
                }
            }
        }
    })]))
    .await;
    let _seeded = seed(
        &harness,
        "custom",
        "aisdk:@ai-sdk/openai-compatible",
        vec![
            catalog_model("plain", "custom", "aisdk:@ai-sdk/openai-compatible"),
            catalog_model("same-package", "custom", "aisdk:@ai-sdk/openai-compatible"),
        ],
    )
    .await;
    let _per_model = seed(&harness, "per-model", "", vec![catalog_model("only-model-package", "per-model", "")]).await;
    let custom = ProviderID("custom".to_owned());
    let per_model = ProviderID("per-model".to_owned());
    let _transformed = harness
        .services
        .model
        .transform(move |editor| {
            for id in ["plain", "same-package"] {
                editor.update(&custom, id, |model| model.variants = variants.clone());
            }
            editor.update(&per_model, "only-model-package", |model| model.variants = variants.clone());
            Ok(())
        })
        .await;
    harness.register(&["config_provider"]).await;

    let expected = ["none", "low", "medium", "xhigh"];
    for (provider_id, id) in [("custom", "plain"), ("custom", "same-package"), ("per-model", "only-model-package")] {
        let model = harness.services.model.get(&ProviderID(provider_id.to_owned()), id).await.unwrap().unwrap();
        assert_eq!(
            model.variants.iter().map(|variant| variant.id.as_str()).collect::<Vec<_>>(),
            expected,
            "{provider_id}/{id}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_provider_respells_inherited_variants_when_config_moves_a_catalog_model_to_another_package() {
    let mut model = catalog_model("anthropic.claude-opus-5-5", "bedrock", "@opencode/ai/providers/amazon-bedrock");
    model["variants"] = json!([{ "id": "high", "body": { "additionalModelRequestFields": { "output_config": { "effort": "high" } } } }]);
    let mut harness = start(Input::new(vec![json!({
        "providers": {
            "bedrock": {
                "models": {
                    "anthropic.claude-opus-5-5": { "package": "@opencode/ai/providers/amazon-bedrock/mantle" }
                }
            }
        }
    })]))
    .await;
    let _seeded = seed(&harness, "bedrock", "@opencode/ai/providers/amazon-bedrock", vec![model]).await;
    harness.register(&["config_provider"]).await;

    let model = harness
        .services
        .model
        .get(&ProviderID("bedrock".to_owned()), "anthropic.claude-opus-5-5")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(model.package.as_deref(), Some("@opencode/ai/providers/amazon-bedrock/mantle/messages"));
    let high = model.variants.iter().find(|variant| variant.id == "high").expect("the high variant");
    assert_eq!(
        json_value(&encode_json(high)),
        json!({
            "id": "high",
            "settings": { "effort": "high", "thinking": { "type": "adaptive", "display": "summarized" } }
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_provider_keeps_a_moved_catalog_model_without_reasoning_variants_free_of_variants() {
    let mut harness = start(Input::new(vec![json!({
        "providers": {
            "bedrock": {
                "models": {
                    "anthropic.claude-plain": { "package": "@opencode/ai/providers/amazon-bedrock/mantle" }
                }
            }
        }
    })]))
    .await;
    let _seeded = seed(
        &harness,
        "bedrock",
        "@opencode/ai/providers/amazon-bedrock",
        vec![catalog_model("anthropic.claude-plain", "bedrock", "@opencode/ai/providers/amazon-bedrock")],
    )
    .await;
    harness.register(&["config_provider"]).await;

    let model =
        harness.services.model.get(&ProviderID("bedrock".to_owned()), "anthropic.claude-plain").await.unwrap().unwrap();
    assert!(model.variants.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_provider_builds_configured_reasoning_variants_after_provider_and_model_package_normalization() {
    let mut harness = start(Input::new(vec![json!({
        "providers": {
            "custom": {
                "package": "aisdk:@ai-sdk/openai",
                "models": {
                    "effort": {
                        "capabilities": { "reasoning": { "supported": true, "effort": ["default", "low", "high"] } },
                        "variants": [{ "id": "high", "headers": { "x-tier": "high" } }]
                    },
                    "gpt": {
                        "package": "aisdk:@ai-sdk/openai",
                        "capabilities": { "reasoning": { "supported": true, "effort": ["low", "high"] } },
                        "variants": [{ "id": "high", "headers": { "x-tier": "high" } }]
                    },
                    "claude-sonnet-4-5": {
                        "package": "aisdk:@ai-sdk/anthropic",
                        "capabilities": { "reasoning": { "supported": true, "budget": { "min": 1024 } } },
                        "limit": { "output": 64000 }
                    },
                    "gemini-3.1-pro": {
                        "package": "aisdk:@ai-sdk/google",
                        "capabilities": { "reasoning": { "supported": true, "effort": ["low", "high"] } }
                    },
                    "magistral-medium": {
                        "package": "aisdk:@ai-sdk/mistral",
                        "capabilities": { "reasoning": { "supported": true, "effort": ["low", "high"] } }
                    },
                    "fixed": { "capabilities": { "reasoning": { "supported": true } } },
                    "plain": { "capabilities": { "reasoning": { "supported": false } } }
                }
            }
        }
    })]))
    .await;
    harness.register(&["config_provider"]).await;

    let custom = ProviderID("custom".to_owned());
    let encoded = |variants: &Vec<ModelVariant>| json_value(&encode_json(variants));
    let expected_effort = json!([
        {
            "id": "low",
            "settings": {
                "reasoningEffort": "low",
                "reasoningSummary": "auto",
                "include": ["reasoning.encrypted_content"]
            }
        },
        {
            "id": "high",
            "settings": {
                "reasoningEffort": "high",
                "reasoningSummary": "auto",
                "include": ["reasoning.encrypted_content"]
            },
            "headers": { "x-tier": "high" }
        }
    ]);

    let effort = harness.services.model.get(&custom, "effort").await.unwrap().unwrap();
    assert_eq!(encoded(&effort.variants), expected_effort);
    let gpt = harness.services.model.get(&custom, "gpt").await.unwrap().unwrap();
    assert_eq!(gpt.package.as_deref(), Some("@opencode/ai/providers/openai"));
    assert_eq!(encoded(&gpt.variants), expected_effort);
    let claude = harness.services.model.get(&custom, "claude-sonnet-4-5").await.unwrap().unwrap();
    assert_eq!(claude.package.as_deref(), Some("@opencode/ai/providers/anthropic"));
    assert_eq!(
        encoded(&claude.variants),
        json!([
            { "id": "high", "settings": { "thinking": { "type": "enabled", "budgetTokens": 16000 } } },
            { "id": "max", "settings": { "thinking": { "type": "enabled", "budgetTokens": 31999 } } }
        ])
    );
    let gemini = harness.services.model.get(&custom, "gemini-3.1-pro").await.unwrap().unwrap();
    assert_eq!(gemini.package.as_deref(), Some("@opencode/ai/providers/google"));
    assert_eq!(
        encoded(&gemini.variants),
        json!([
            { "id": "low", "settings": { "thinkingConfig": { "includeThoughts": true, "thinkingLevel": "low" } } },
            { "id": "high", "settings": { "thinkingConfig": { "includeThoughts": true, "thinkingLevel": "high" } } }
        ])
    );
    let magistral = harness.services.model.get(&custom, "magistral-medium").await.unwrap().unwrap();
    assert_eq!(magistral.package.as_deref(), Some("@opencode/ai/providers/mistral"));
    assert_eq!(
        encoded(&magistral.variants),
        json!([
            { "id": "low", "settings": { "reasoningEffort": "low" } },
            { "id": "high", "settings": { "reasoningEffort": "high" } }
        ])
    );
    let fixed = harness.services.model.get(&custom, "fixed").await.unwrap().unwrap();
    assert!(fixed.variants.is_empty());
    let plain = harness.services.model.get(&custom, "plain").await.unwrap().unwrap();
    assert!(plain.variants.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_provider_keeps_configured_model_variant_bodies_unchanged() {
    let mut harness = start(Input::new(vec![json!({
        "providers": {
            "opencode": {
                "package": "aisdk:@ai-sdk/openai",
                "settings": { "baseURL": "https://opencode.test/v1" },
                "models": {
                    "alpha-gpt-next": {
                        "variants": [
                            {
                                "id": "high",
                                "body": {
                                    "reasoningEffort": "high",
                                    "reasoningSummary": "auto",
                                    "include": ["reasoning.encrypted_content"]
                                }
                            }
                        ]
                    }
                }
            }
        }
    })]))
    .await;
    harness.register(&["config_provider"]).await;

    let model =
        harness.services.model.get(&ProviderID("opencode".to_owned()), "alpha-gpt-next").await.unwrap().unwrap();
    assert_eq!(model.variants.len(), 1);
    assert_eq!(model.variants[0].id, "high");
    assert_eq!(
        json_value(&encode_json(model.variants[0].body.as_ref().expect("the variant body"))),
        json!({
            "reasoningEffort": "high",
            "reasoningSummary": "auto",
            "include": ["reasoning.encrypted_content"]
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_provider_keeps_layered_model_variant_bodies_unchanged() {
    let mut harness = start(Input::new(vec![
        json!({
            "providers": {
                "opencode": {
                    "package": "aisdk:@ai-sdk/openai",
                    "settings": { "baseURL": "https://opencode.test/v1" }
                }
            }
        }),
        json!({
            "providers": {
                "opencode": {
                    "models": {
                        "alpha-gpt-next": { "variants": [{ "id": "high", "body": { "reasoningEffort": "high" } }] }
                    }
                }
            }
        }),
    ]))
    .await;
    harness.register(&["config_provider"]).await;

    let model =
        harness.services.model.get(&ProviderID("opencode".to_owned()), "alpha-gpt-next").await.unwrap().unwrap();
    let high = model.variants.iter().find(|variant| variant.id == "high").expect("the high variant");
    assert_eq!(
        json_value(&encode_json(high.body.as_ref().expect("the variant body"))),
        json!({ "reasoningEffort": "high" })
    );
}
