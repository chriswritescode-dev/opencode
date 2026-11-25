use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use opencode_bus::{Bus, Event, ListenerHandle};
use opencode_catalog::aisdk_native::aisdk;
use opencode_catalog::credential::CreateInput;
use opencode_catalog::model::{self, ModelService};
use opencode_contract::codec::{Encode, decode_json, encode_json};
use opencode_contract::generated::{
    CredentialKey, CredentialKeyType, CredentialValue, IntegrationID, ModelCost, ModelCostCache, ModelRef,
    ModelSettings, ModelVariant, ModelVariantSettings, MoneyUSDPerMillionTokens, ProviderID, ProviderInfo,
    ProviderInfoActivation, ProviderSettings,
};
use serde_json::{Value, json};

use crate::provider::{Harness, setup, setup_with_env};

fn models(harness: &Harness, values: HashMap<String, String>) -> Arc<ModelService> {
    ModelService::new(
        harness.providers.clone(),
        harness.bus.clone(),
        harness.location.clone(),
        crate::provider::config_env(&values),
    )
}

fn empty_provider(id: &str) -> ProviderInfo {
    ProviderInfo {
        id: ProviderID(id.to_owned()),
        canonical: None,
        integration_id: None,
        name: id.to_owned(),
        activation: ProviderInfoActivation::Auto,
        package: String::new(),
        compatibility: None,
        settings: None,
        headers: None,
        body: None,
    }
}

fn enabled_provider(id: &str) -> ProviderInfo {
    ProviderInfo { activation: ProviderInfoActivation::Enabled, ..empty_provider(id) }
}

fn encoded(value: &impl Encode) -> Value {
    serde_json::from_str(&encode_json(value)).expect("encode contract value")
}

fn capture(bus: &Bus) -> (Arc<Mutex<Vec<Event>>>, ListenerHandle) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let handle = bus.listen(Box::new(move |event: &Event| {
        sink.lock().expect("lock the events").push(event.clone());
        Ok(())
    }));
    (events, handle)
}

fn credential_input(integration_id: &IntegrationID, key: &str, label: &str) -> CreateInput {
    CreateInput {
        id: None,
        integration_id: integration_id.clone(),
        value: CredentialValue::Key(CredentialKey {
            r#type: CredentialKeyType::Key,
            key: key.to_owned(),
            metadata: None,
        }),
        label: Some(label.to_owned()),
        activate: None,
    }
}

async fn settle(mut condition: impl FnMut() -> bool) {
    for _ in 0..100 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for catalog events");
}

fn cost(input: f64, output: f64) -> ModelCost {
    ModelCost {
        tier: None,
        input: MoneyUSDPerMillionTokens(input),
        output: MoneyUSDPerMillionTokens(output),
        cache: ModelCostCache { read: MoneyUSDPerMillionTokens(0.0), write: MoneyUSDPerMillionTokens(0.0) },
    }
}

#[tokio::test]
async fn parse_splits_at_the_first_slash() {
    for (input, provider_id, model_id) in [
        ("vendor/model", "vendor", "model"),
        ("vendor/team/model", "vendor", "team/model"),
        ("vendor", "vendor", ""),
        ("", "", ""),
        ("/model", "", "model"),
        ("vendor/", "vendor", ""),
        ("vendor//model/", "vendor", "/model/"),
    ] {
        assert_eq!(model::parse(input), (ProviderID(provider_id.to_owned()), model_id.to_owned()));
    }
}

#[tokio::test]
async fn model_ref_decodes_with_and_without_a_variant() {
    let plain = decode_json::<ModelRef>(r#"{"id":"claude-sonnet","providerID":"anthropic"}"#)
        .expect("decode a model ref without a variant");
    assert_eq!(plain.id, "claude-sonnet");
    assert_eq!(plain.provider_id, ProviderID("anthropic".to_owned()));
    assert_eq!(plain.variant, None);

    let variant = decode_json::<ModelRef>(r#"{"id":"claude-sonnet","providerID":"anthropic","variant":"high"}"#)
        .expect("decode a model ref with a variant");
    assert_eq!(variant.id, "claude-sonnet");
    assert_eq!(variant.provider_id, ProviderID("anthropic".to_owned()));
    assert_eq!(variant.variant, Some("high".to_owned()));
}

#[tokio::test]
async fn persists_direct_edits_to_models_returned_by_list_and_get() {
    let harness = setup();
    let models = models(&harness, HashMap::new());
    let provider_id = ProviderID("direct".to_owned());
    let _provider_registration = harness
        .providers
        .transform({
            let provider_id = provider_id.clone();
            move |editor| {
                editor.add(enabled_provider(&provider_id.0), &[], None);
                editor.models_update(&provider_id, "listed", |_| {});
                editor.models_update(&provider_id, "fetched", |_| {});
                Ok(())
            }
        })
        .await;
    let _model_registration = models
        .transform({
            let provider_id = provider_id.clone();
            move |editor| {
                for model in editor.list(Some(&provider_id)) {
                    model.limit.context = 4096.0;
                }
                editor.get(&provider_id, "fetched").expect("fetched model").capabilities.input.push("pdf".to_owned());
                Ok(())
            }
        })
        .await;

    let listed = models.get(&provider_id, "listed").await.expect("get listed model").expect("listed model");
    assert_eq!(listed.limit.context, 4096.0);
    assert_eq!(encoded(&listed.capabilities)["input"], json!(["text", "image"]));

    let fetched = models.get(&provider_id, "fetched").await.expect("get fetched model").expect("fetched model");
    assert_eq!(fetched.limit.context, 4096.0);
    assert_eq!(encoded(&fetched.capabilities)["input"], json!(["text", "image", "pdf"]));

    let snapshot = harness.providers.snapshot().await.expect("provider snapshot");
    let definitions = &snapshot.records.get("direct").expect("direct provider").models;
    assert_eq!(definitions.get("listed").expect("listed definition").limit.context, 200_000.0);
    assert_eq!(definitions.get("fetched").expect("fetched definition").limit.context, 200_000.0);
}

#[tokio::test]
async fn gives_foreign_definitions_the_registering_providers_identity() {
    let harness = setup();
    let models = models(&harness, HashMap::new());
    let source = ProviderID("source".to_owned());
    let mirror = ProviderID("mirror".to_owned());
    let _provider_registration = harness
        .providers
        .transform({
            let source = source.clone();
            let mirror = mirror.clone();
            move |editor| {
                editor.add(enabled_provider(&source.0), &[], None);
                editor.models_update(&source, "chat", |_| {});
                let definitions = vec![editor.get(&source).expect("source provider").models["chat"].clone()];
                editor.add(enabled_provider(&mirror.0), &definitions, None);
                Ok(())
            }
        })
        .await;

    let mut available: Vec<String> =
        models.available(None).await.expect("available models").into_iter().map(|model| model.provider_id.0).collect();
    available.sort();
    assert_eq!(available, ["mirror", "source"]);
    let model = models.get(&mirror, "chat").await.expect("get mirrored model").expect("mirrored model");
    assert_eq!(model.id, "chat");
    assert_eq!(model.provider_id, mirror);

    let snapshot = harness.providers.snapshot().await.expect("provider snapshot");
    assert_eq!(snapshot.records.get("source").expect("source provider").models["chat"].provider_id, source);
}

#[tokio::test]
async fn keeps_materialized_models_when_another_provider_becomes_available() {
    let harness = setup();
    let models = models(&harness, HashMap::new());
    let existing = ProviderID("existing".to_owned());
    let added = ProviderID("added".to_owned());
    let _integration_registration = harness
        .integrations
        .transform({
            let added = added.clone();
            move |editor| {
                editor.update(&IntegrationID(added.0.clone()), |_| {});
                Ok(())
            }
        })
        .await;
    let _provider_registration = harness
        .providers
        .transform({
            let existing = existing.clone();
            let added = added.clone();
            move |editor| {
                editor.add(enabled_provider(&existing.0), &[], None);
                editor.models_update(&existing, "edited", |_| {});
                editor.models_update(&existing, "untouched", |_| {});
                editor.add(empty_provider(&added.0), &[], None);
                editor.models_update(&added, "chat", |_| {});
                Ok(())
            }
        })
        .await;
    let _model_registration = models
        .transform({
            let existing = existing.clone();
            move |editor| {
                editor.update(&existing, "edited", |model| model.limit.context = 1.0);
                Ok(())
            }
        })
        .await;
    let before = models.get(&existing, "untouched").await.expect("get untouched model").expect("untouched model");

    harness
        .credentials
        .create(credential_input(&IntegrationID(added.0.clone()), "secret", "default"))
        .expect("credential");

    let mut available: Vec<String> =
        models.available(None).await.expect("available models").into_iter().map(|model| model.provider_id.0).collect();
    available.sort();
    assert_eq!(available, ["added", "existing", "existing"]);
    let after = models.get(&existing, "untouched").await.expect("get untouched model").expect("untouched model");
    assert_eq!(after, before);
    let edited = models.get(&existing, "edited").await.expect("get edited model").expect("edited model");
    assert_eq!(edited.limit.context, 1.0);
}

#[tokio::test]
async fn uses_model_package_settings_over_provider_settings() {
    let harness = setup();
    let models = models(&harness, HashMap::new());
    let provider_id = ProviderID("test".to_owned());
    let _provider_registration = harness
        .providers
        .transform({
            let provider_id = provider_id.clone();
            move |editor| {
                editor.update(&provider_id, |provider| {
                    provider.package = aisdk("custom-sdk");
                    provider.settings = Some(
                        decode_json::<ProviderSettings>(r#"{"baseURL":"https://provider.example.com"}"#)
                            .expect("decode provider settings"),
                    );
                });
                Ok(())
            }
        })
        .await;
    let _model_registration = models
        .transform({
            let provider_id = provider_id.clone();
            move |editor| {
                editor.update(&provider_id, "model", |model| {
                    model.model_id = "upstream-model".to_owned();
                    model.package = Some(aisdk("custom-sdk"));
                    model.settings = Some(
                        decode_json::<ModelSettings>(r#"{"baseURL":"https://override.example.com"}"#)
                            .expect("decode model settings"),
                    );
                });
                Ok(())
            }
        })
        .await;

    let model = models.get(&provider_id, "model").await.expect("get model").expect("model");
    let value = encoded(&model);
    assert_eq!(value["modelID"], json!("upstream-model"));
    assert_eq!(value["package"], json!("aisdk:custom-sdk"));
    assert_eq!(value["settings"], json!({"baseURL": "https://override.example.com"}));
}

#[tokio::test]
async fn resolves_default_model_package_settings_from_the_provider() {
    let harness = setup();
    let models = models(&harness, HashMap::new());
    let provider_id = ProviderID("test".to_owned());
    let _provider_registration = harness
        .providers
        .transform({
            let provider_id = provider_id.clone();
            move |editor| {
                editor.update(&provider_id, |provider| {
                    provider.package = aisdk("custom-sdk");
                    provider.settings = Some(
                        decode_json::<ProviderSettings>(r#"{"baseURL":"https://provider.example.com"}"#)
                            .expect("decode provider settings"),
                    );
                });
                editor.models_update(&provider_id, "model", |_| {});
                Ok(())
            }
        })
        .await;

    let model = models.get(&provider_id, "model").await.expect("get model").expect("model");
    let value = encoded(&model);
    assert_eq!(value["package"], json!("aisdk:custom-sdk"));
    assert_eq!(value["settings"], json!({"baseURL": "https://provider.example.com"}));
}

#[tokio::test]
async fn rewrites_ai_sdk_packages_and_their_options_on_write() {
    let harness = setup();
    let models = models(&harness, HashMap::new());
    let provider_id = ProviderID("bedrock".to_owned());
    let gitlab = ProviderID("gitlab".to_owned());
    let _provider_registration = harness
        .providers
        .transform({
            let provider_id = provider_id.clone();
            let gitlab = gitlab.clone();
            move |editor| {
                editor.update(&provider_id, |provider| {
                    provider.package = aisdk("@ai-sdk/amazon-bedrock");
                    provider.settings = Some(
                        decode_json::<ProviderSettings>(r#"{"region":"us-east-1","extraBody":{"tag":"provider"}}"#)
                            .expect("decode provider settings"),
                    );
                });
                editor.models_update(&provider_id, "claude", |_| {});
                editor.update(&gitlab, |provider| {
                    provider.package = aisdk("@ai-sdk/gitlab");
                    provider.settings = Some(
                        decode_json::<ProviderSettings>(r#"{"extraBody":{"kept":true}}"#)
                            .expect("decode provider settings"),
                    );
                });
                Ok(())
            }
        })
        .await;
    let _model_registration = models
        .transform({
            let provider_id = provider_id.clone();
            move |editor| {
                editor.update(&provider_id, "claude", |model| {
                    model.model_id = "anthropic.claude-opus-4-8".to_owned();
                    model.package = Some(aisdk("@ai-sdk/amazon-bedrock"));
                    model.settings = Some(
                        decode_json::<ModelSettings>(
                            r#"{"reasoningConfig":{"type":"adaptive","maxReasoningEffort":"high"}}"#,
                        )
                        .expect("decode model settings"),
                    );
                    model.variants = vec![ModelVariant {
                        id: "max".to_owned(),
                        settings: Some(
                            decode_json::<ModelVariantSettings>(
                                r#"{"reasoningConfig":{"type":"adaptive","maxReasoningEffort":"max"}}"#,
                            )
                            .expect("decode variant settings"),
                        ),
                        headers: None,
                        body: None,
                    }];
                });
                Ok(())
            }
        })
        .await;

    let provider = harness.providers.get(&provider_id).await.expect("get provider").expect("provider");
    let value = encoded(&provider);
    assert_eq!(value["package"], json!("@opencode/ai/providers/amazon-bedrock"));
    assert_eq!(value["settings"], json!({"region": "us-east-1"}));
    assert_eq!(value["body"], json!({"tag": "provider"}));

    let model = models.get(&provider_id, "claude").await.expect("get model").expect("model");
    let value = encoded(&model);
    assert_eq!(value["package"], json!("@opencode/ai/providers/amazon-bedrock"));
    assert!(value["settings"].get("reasoningConfig").is_none());
    assert_eq!(
        value["body"]["additionalModelRequestFields"],
        json!({"thinking": {"type": "adaptive"}, "output_config": {"effort": "high"}})
    );
    assert_eq!(
        value["variants"],
        json!([{
            "id": "max",
            "settings": {},
            "body": {
                "additionalModelRequestFields": {"thinking": {"type": "adaptive"}, "output_config": {"effort": "max"}}
            }
        }])
    );

    let gitlab = harness.providers.get(&gitlab).await.expect("get gitlab provider").expect("gitlab provider");
    let value = encoded(&gitlab);
    assert_eq!(value["package"], json!("aisdk:@ai-sdk/gitlab"));
    assert_eq!(value["settings"], json!({"extraBody": {"kept": true}}));
}

#[tokio::test]
async fn keeps_settings_scoped_while_resolving_request_overlay_merges() {
    let harness = setup();
    let models = models(&harness, HashMap::new());
    let provider_id = ProviderID("test".to_owned());
    let _provider_registration = harness
        .providers
        .transform({
            let provider_id = provider_id.clone();
            move |editor| {
                editor.update(&provider_id, |provider| {
                    provider.settings = Some(
                        decode_json::<ProviderSettings>(r#"{"provider":true,"shared":"provider"}"#)
                            .expect("decode provider settings"),
                    );
                    provider.headers =
                        Some(decode_json(r#"{"provider":"provider","shared":"provider"}"#).expect("decode headers"));
                    provider.body = Some(decode_json(r#"{"provider":true,"shared":"provider"}"#).expect("decode body"));
                });
                Ok(())
            }
        })
        .await;
    let _model_registration = models
        .transform({
            let provider_id = provider_id.clone();
            move |editor| {
                editor.update(&provider_id, "model", |model| {
                    model.settings = Some(
                        decode_json::<ModelSettings>(r#"{"model":true,"shared":"model"}"#)
                            .expect("decode model settings"),
                    );
                    model.headers = Some(decode_json(r#"{"model":"model","shared":"model"}"#).expect("decode headers"));
                    model.body = Some(decode_json(r#"{"model":true,"shared":"model"}"#).expect("decode body"));
                });
                Ok(())
            }
        })
        .await;

    let provider = harness.providers.get(&provider_id).await.expect("get provider").expect("provider");
    assert_eq!(encoded(&provider)["settings"], json!({"provider": true, "shared": "provider"}));
    let model = models.get(&provider_id, "model").await.expect("get model").expect("model");
    let value = encoded(&model);
    assert_eq!(value["settings"], json!({"provider": true, "shared": "model", "model": true}));
    assert_eq!(value["headers"], json!({"provider": "provider", "shared": "model", "model": "model"}));
    assert_eq!(value["body"], json!({"provider": true, "shared": "model", "model": true}));
}

#[tokio::test]
async fn falls_back_to_newest_available_model_when_no_default_is_configured() {
    let harness = setup();
    let models = models(&harness, HashMap::new());
    let provider_id = ProviderID("test".to_owned());
    let _provider_registration = harness
        .providers
        .transform({
            let provider_id = provider_id.clone();
            move |editor| {
                editor.update(&provider_id, |_| {});
                editor.models_update(&provider_id, "old", |model| model.time.released = 1000.0);
                editor.models_update(&provider_id, "new", |model| model.time.released = 2000.0);
                editor.models_update(&provider_id, "newest-video", |model| {
                    model.capabilities.input = vec!["text".to_owned(), "image".to_owned()];
                    model.capabilities.output = vec!["video".to_owned()];
                    model.time.released = 3000.0;
                });
                editor.models_update(&provider_id, "newest-stt", |model| {
                    model.capabilities.input = vec!["audio".to_owned()];
                    model.capabilities.output = vec!["text".to_owned()];
                    model.time.released = 4000.0;
                });
                Ok(())
            }
        })
        .await;

    let model = models.default_model().await.expect("default model").expect("a default model");
    assert_eq!(model.id, "new");
}

#[tokio::test]
async fn defaults_to_big_pickle_when_only_free_opencode_models_are_available() {
    let harness = setup();
    let models = models(&harness, HashMap::new());
    let provider_id = ProviderID("opencode".to_owned());
    let _provider_registration = harness
        .providers
        .transform({
            let provider_id = provider_id.clone();
            move |editor| {
                editor.update(&provider_id, |_| {});
                for (index, id) in
                    ["muse-spark-1.3-contributor-free", "new-free", "deepseek-v4.1-flash-free", "big-pickle"]
                        .into_iter()
                        .enumerate()
                {
                    editor.models_update(&provider_id, id, |model| {
                        model.time.released = 4000.0 - index as f64 * 1000.0;
                    });
                }
                Ok(())
            }
        })
        .await;

    let available: Vec<String> =
        models.available(None).await.expect("available models").into_iter().map(|model| model.id).collect();
    assert_eq!(available, ["big-pickle", "deepseek-v4.1-flash-free", "muse-spark-1.3-contributor-free", "new-free"]);
    let model = models.default_model().await.expect("default model").expect("a default model");
    assert_eq!(model.id, "big-pickle");
}

#[tokio::test]
async fn keeps_the_newest_default_when_a_paid_model_is_available() {
    let harness = setup();
    let models = models(&harness, HashMap::new());
    let provider_id = ProviderID("opencode".to_owned());
    let _provider_registration = harness
        .providers
        .transform({
            let provider_id = provider_id.clone();
            move |editor| {
                editor.update(&provider_id, |_| {});
                editor.models_update(&provider_id, "big-pickle", |model| model.time.released = 1000.0);
                editor.models_update(&provider_id, "paid", |model| {
                    model.time.released = 2000.0;
                    model.cost = vec![cost(1.0, 1.0)];
                });
                Ok(())
            }
        })
        .await;

    let model = models.default_model().await.expect("default model").expect("a default model");
    assert_eq!(model.id, "paid");
}

#[tokio::test]
async fn uses_a_transform_provided_default_until_that_transform_is_replaced() {
    let harness = setup();
    let models = models(&harness, HashMap::new());
    let provider_id = ProviderID("test".to_owned());
    let _provider_registration = harness
        .providers
        .transform({
            let provider_id = provider_id.clone();
            move |editor| {
                editor.update(&provider_id, |_| {});
                editor.models_update(&provider_id, "old", |model| model.time.released = 1000.0);
                editor.models_update(&provider_id, "new", |model| model.time.released = 2000.0);
                Ok(())
            }
        })
        .await;
    let configured = Arc::new(AtomicBool::new(true));
    let _model_registration = models
        .transform({
            let configured = configured.clone();
            let provider_id = provider_id.clone();
            move |editor| {
                if configured.load(Ordering::SeqCst) {
                    editor.set_default_selection(&provider_id, "old");
                }
                Ok(())
            }
        })
        .await;

    let model = models.default_model().await.expect("default model").expect("a default model");
    assert_eq!(model.id, "old");

    configured.store(false, Ordering::SeqCst);
    models.reload().await.expect("reload models");
    let model = models.default_model().await.expect("default model").expect("a default model");
    assert_eq!(model.id, "new");
}

#[tokio::test]
async fn ignores_a_configured_default_on_a_disabled_provider() {
    let harness = setup();
    let models = models(&harness, HashMap::new());
    let disabled = ProviderID("disabled".to_owned());
    let enabled = ProviderID("enabled".to_owned());
    let _provider_registration = harness
        .providers
        .transform({
            let disabled = disabled.clone();
            let enabled = enabled.clone();
            move |editor| {
                editor.update(&disabled, |provider| provider.activation = ProviderInfoActivation::Disabled);
                editor.models_update(&disabled, "configured", |_| {});
                editor.update(&enabled, |_| {});
                editor.models_update(&enabled, "fallback", |_| {});
                Ok(())
            }
        })
        .await;
    let _model_registration = models
        .transform({
            let disabled = disabled.clone();
            move |editor| {
                editor.set_default_selection(&disabled, "configured");
                Ok(())
            }
        })
        .await;

    let model = models.default_model().await.expect("default model").expect("a default model");
    assert_eq!(model.provider_id, enabled);
    assert_eq!(model.id, "fallback");
}

#[tokio::test]
async fn small_model_uses_the_newest_release_in_the_first_matching_family() {
    let harness = setup();
    let models = models(&harness, HashMap::new());
    let provider_id = ProviderID("test".to_owned());
    let _provider_registration = harness
        .providers
        .transform({
            let provider_id = provider_id.clone();
            move |editor| {
                editor.update(&provider_id, |_| {});
                editor.models_update(&provider_id, "newer-flash", |model| {
                    model.family = Some("gemini-flash".to_owned());
                    model.capabilities.input = vec!["text".to_owned()];
                    model.capabilities.output = vec!["text".to_owned()];
                    model.time.released = 3000.0;
                });
                editor.models_update(&provider_id, "older-luna", |model| {
                    model.family = Some("gpt-luna".to_owned());
                    model.capabilities.input = vec!["text".to_owned()];
                    model.capabilities.output = vec!["text".to_owned()];
                    model.time.released = 1000.0;
                });
                editor.models_update(&provider_id, "newer-luna", |model| {
                    model.family = Some("gpt-luna".to_owned());
                    model.capabilities.input = vec!["text".to_owned()];
                    model.capabilities.output = vec!["text".to_owned()];
                    model.time.released = 2000.0;
                });
                Ok(())
            }
        })
        .await;

    let model = models.small(&provider_id).await.expect("small model").expect("a small model");
    assert_eq!(model.id, "newer-luna");
}

#[tokio::test]
async fn small_model_returns_none_without_a_matching_family() {
    let harness = setup();
    let models = models(&harness, HashMap::new());
    let provider_id = ProviderID("test".to_owned());
    let _provider_registration = harness
        .providers
        .transform({
            let provider_id = provider_id.clone();
            move |editor| {
                editor.update(&provider_id, |_| {});
                editor.models_update(&provider_id, "large", |model| {
                    model.family = Some("gpt".to_owned());
                    model.capabilities.input = vec!["text".to_owned()];
                    model.capabilities.output = vec!["text".to_owned()];
                });
                Ok(())
            }
        })
        .await;

    assert!(models.small(&provider_id).await.expect("small model").is_none());
}

#[tokio::test]
async fn reuses_the_model_catalog_across_credential_switches() {
    let harness = setup();
    let models = models(&harness, HashMap::new());
    let provider_id = ProviderID("switchable".to_owned());
    let integration_id = IntegrationID("switchable".to_owned());
    let _integration_registration = harness
        .integrations
        .transform({
            let integration_id = integration_id.clone();
            move |editor| {
                editor.update(&integration_id, |_| {});
                Ok(())
            }
        })
        .await;
    let _provider_registration = harness
        .providers
        .transform({
            let provider_id = provider_id.clone();
            move |editor| {
                editor.add(empty_provider(&provider_id.0), &[], None);
                editor.models_update(&provider_id, "chat", |_| {});
                Ok(())
            }
        })
        .await;
    assert!(models.available(None).await.expect("available models").is_empty());

    let (events, _listener) = capture(&harness.bus);
    let first = harness
        .credentials
        .create(credential_input(&integration_id, "first", "First"))
        .expect("create the first credential");
    assert_eq!(models.available(None).await.expect("available models").len(), 1);
    settle(|| events.lock().expect("lock the events").iter().any(|event| event.r#type() == "model.updated")).await;
    let materialized = models.snapshot().await.expect("model snapshot");

    let second = harness
        .credentials
        .create(credential_input(&integration_id, "second", "Second"))
        .expect("create the second credential");
    assert_eq!(models.available(None).await.expect("available models").len(), 1);
    assert!(Arc::ptr_eq(&models.snapshot().await.expect("model snapshot"), &materialized));

    harness.credentials.activate(&first.id).expect("activate the first credential");
    assert_eq!(models.available(None).await.expect("available models").len(), 1);
    assert!(Arc::ptr_eq(&models.snapshot().await.expect("model snapshot"), &materialized));

    harness.credentials.remove(&first.id).expect("remove the first credential");
    assert_eq!(models.available(None).await.expect("available models").len(), 1);
    assert!(Arc::ptr_eq(&models.snapshot().await.expect("model snapshot"), &materialized));

    harness.credentials.remove(&second.id).expect("remove the second credential");
    assert!(models.available(None).await.expect("available models").is_empty());
    assert!(!Arc::ptr_eq(&models.snapshot().await.expect("model snapshot"), &materialized));
    settle(|| {
        let events = events.lock().expect("lock the events");
        let model_index = events.iter().rposition(|event| event.r#type() == "model.updated");
        let credential_index = events.iter().rposition(|event| event.r#type() == "credential.updated");
        matches!(
            (model_index, credential_index),
            (Some(model_index), Some(credential_index)) if model_index > credential_index
        )
    })
    .await;
}

#[tokio::test]
async fn free_models_follow_opencode_disable_free_models() {
    for flag in [None, Some("0"), Some("1"), Some("true")] {
        for provider in ["opencode", "opencode-go"] {
            let mut values = HashMap::new();
            if let Some(flag) = flag {
                values.insert("OPENCODE_DISABLE_FREE_MODELS".to_owned(), flag.to_owned());
            }
            let harness = setup_with_env(values.clone());
            let models = models(&harness, values);
            let provider_id = ProviderID(provider.to_owned());
            let other = ProviderID("other".to_owned());
            let _provider_registration = harness
                .providers
                .transform({
                    let provider_id = provider_id.clone();
                    let other = other.clone();
                    move |editor| {
                        editor.update(&provider_id, |_| {});
                        editor.models_update(&provider_id, "big-pickle", |model| {
                            model.time.released = 3000.0;
                            model.family = Some("gpt-luna".to_owned());
                            model.capabilities.input = vec!["text".to_owned()];
                            model.capabilities.output = vec!["text".to_owned()];
                        });
                        editor.models_update(&provider_id, "paid", |model| {
                            model.time.released = 2000.0;
                            model.cost = vec![cost(1.0, 0.0)];
                        });
                        editor.update(&other, |_| {});
                        editor.models_update(&other, "local", |_| {});
                        Ok(())
                    }
                })
                .await;
            let _model_registration = models
                .transform({
                    let provider_id = provider_id.clone();
                    move |editor| {
                        editor.set_default_selection(&provider_id, "big-pickle");
                        Ok(())
                    }
                })
                .await;

            let disabled = matches!(flag, Some("1" | "true"));
            let all = models.all().await.expect("all models");
            let ids: Vec<&str> = all.iter().map(|model| model.id.as_str()).collect();
            if disabled {
                assert_eq!(ids, ["paid", "local"]);
            } else {
                assert_eq!(ids, ["big-pickle", "paid", "local"]);
            }
            assert_eq!(models.available(None).await.expect("available models"), all);
            let default = models.default_model().await.expect("default model").expect("a default model");
            assert_eq!(default.id, if disabled { "paid" } else { "big-pickle" });
            assert_eq!(models.get(&provider_id, "big-pickle").await.expect("get model").is_some(), !disabled);
            let small = models.small(&provider_id).await.expect("small model");
            assert_eq!(small.is_some(), !disabled);
            if let Some(small) = small {
                assert_eq!(small.id, "big-pickle");
            }
            if disabled {
                let _restore = models
                    .transform({
                        let provider_id = provider_id.clone();
                        move |editor| {
                            editor.update(&provider_id, "big-pickle", |model| model.enabled = true);
                            editor.remove(&provider_id, "paid");
                            Ok(())
                        }
                    })
                    .await;
                let default = models.default_model().await.expect("default model").expect("a default model");
                assert_eq!(default.provider_id, other);
                assert!(models.get(&provider_id, "big-pickle").await.expect("get model").is_none());
            }
        }
    }
}
