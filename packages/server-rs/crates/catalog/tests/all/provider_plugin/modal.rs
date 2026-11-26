use std::time::Duration;

use opencode_catalog::provider_plugin::modal::{ID, models, models_key};
use opencode_contract::codec::{decode_json, encode_json};
use opencode_contract::generated::{
    ConnectionCredentialInfo, ConnectionCredentialInfoMethod, ConnectionCredentialInfoType, ConnectionEnvInfo,
    ConnectionEnvInfoType, ConnectionInfo, CredentialID, CredentialKey, CredentialKeyType, CredentialValue, ModelInfo,
    ModelInfoStatus, ModelReasoningField, ModelReasoningFieldString2, MoneyUSDPerMillionTokens, ProviderID,
};
use serde_json::{Value, json};

use crate::builtin::{Harness, Input, start};
use crate::provider_plugin::support::{
    Exchange, RecordedSender, Reply, Routes, plugin_context_with, provider_settings_from,
};

const PROVIDER_ID: &str = "modal";

const BASE: &str = "https://modal.example/v1";

fn provider_id() -> ProviderID {
    ProviderID(PROVIDER_ID.to_owned())
}

fn authorization(request: &opencode_ai::route::executor::PreparedHttp) -> Option<String> {
    request.headers.iter().find(|(name, _)| name.eq_ignore_ascii_case("authorization")).map(|(_, value)| value.clone())
}

fn credential(key: &str) -> CredentialValue {
    CredentialValue::Key(CredentialKey { r#type: CredentialKeyType::Key, key: key.to_owned(), metadata: None })
}

fn template(id: &str, overrides: Value) -> ModelInfo {
    let base = json!({
        "id": id,
        "modelID": id,
        "providerID": PROVIDER_ID,
        "name": format!("{id} catalog"),
        "family": "catalog-family",
        "capabilities": { "tools": true, "input": ["text", "image"], "output": ["text"] },
        "variants": [],
        "time": { "released": 0 },
        "cost": [],
        "status": "active",
        "enabled": true,
        "limit": { "context": 200000, "output": 32000 },
    });
    let mut base = base.as_object().expect("the template object").clone();
    if let Value::Object(overrides) = overrides {
        base.extend(overrides);
    }
    decode_json(&Value::Object(base).to_string()).expect("the template model")
}

fn settings(value: &ModelInfo) -> Value {
    serde_json::from_str(&encode_json(value.settings.as_ref().expect("the model settings")))
        .expect("the model settings json")
}

async fn seed_provider(harness: &Harness, base_url: &str) -> opencode_location::state::Registration {
    let id = provider_id();
    let settings = provider_settings_from(json!({ "baseURL": base_url }));
    harness
        .services
        .provider
        .transform(move |editor| {
            editor.update(&id, |provider| {
                provider.package = models::PACKAGE.to_owned();
                provider.settings = Some(settings.clone());
            });
            Ok(())
        })
        .await
}

async fn wait_for_model(harness: &Harness, id: &str) -> ModelInfo {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(model) = harness.services.model.get(&provider_id(), id).await.unwrap_or(None) {
                return model;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the model {id} did not appear"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn modal_plugin_is_registered() {
    assert_eq!(ID, "opencode.provider.modal");
    assert!(opencode_catalog::provider_plugin::ORDER.iter().any(|slot| slot.id() == ID));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn maps_live_modal_models_onto_catalog_templates() {
    let sender = RecordedSender::ordered(vec![Exchange::new(
        "GET",
        &format!("{BASE}/models"),
        200,
        json!({
            "data": [
                {
                    "id": "live-model",
                    "base_model_id": "base-model",
                    "name": "Live Model",
                    "input_modalities": ["text", "image"],
                    "output_modalities": ["text"],
                    "context_length": 128000,
                    "max_output_length": 8192,
                    "pricing": { "prompt": "0.000001", "completion": 0.000002, "input_cache_read": "0.0000002" },
                    "supported_sampling_parameters": ["temperature"],
                    "supported_features": ["tools", "reasoning"],
                    "reasoning_options": [{ "type": "effort", "values": ["low", "high", null] }],
                    "interleaved": { "field": "reasoning_content" },
                },
                { "id": "standalone", "context_length": 64000 },
                { "id": "malformed", "context_length": "huge" },
            ],
        })
        .to_string(),
    )]);

    let base = template("base-model", json!({}));
    let stale = template("stale", json!({}));
    let discovered = models::get(&sender, BASE, "test-key", &[base, stale]).await.expect("the modal models");

    let requests = sender.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].url, format!("{BASE}/models"));
    assert_eq!(authorization(&requests[0]).as_deref(), Some("Bearer test-key"));

    assert!(!discovered.contains_key("stale"));
    assert!(!discovered.contains_key("malformed"));

    let model = discovered.get("live-model").expect("the live model");
    assert_eq!(model.name, "Live Model");
    assert_eq!(model.family.as_deref(), Some("catalog-family"));
    assert_eq!(model.provider_id, provider_id());
    assert_eq!(model.model_id, "live-model");
    assert_eq!(model.package.as_deref(), Some(models::PACKAGE));
    assert_eq!(settings(model).get("baseURL"), Some(&json!(BASE)));
    assert_eq!(
        model.compatibility.as_ref().and_then(|compatibility| compatibility.reasoning_field.clone()),
        Some(ModelReasoningField::String2(ModelReasoningFieldString2::ReasoningContent))
    );
    assert!(model.capabilities.tools);
    assert_eq!(model.capabilities.input, ["text", "image"]);
    assert_eq!(model.capabilities.output, ["text"]);
    assert_eq!(model.cost[0].input, MoneyUSDPerMillionTokens(1.0));
    assert_eq!(model.cost[0].output, MoneyUSDPerMillionTokens(2.0));
    assert!((model.cost[0].cache.read.0 - 0.2).abs() < 1e-10);
    assert_eq!(model.cost[0].cache.write, MoneyUSDPerMillionTokens(0.0));
    assert_eq!(model.limit.context, 128000.0);
    assert_eq!(model.limit.output, 8192.0);
    assert_eq!(model.limit.input, None);
    let variants: Vec<&str> = model.variants.iter().map(|variant| variant.id.as_str()).collect();
    assert_eq!(variants, ["low", "high", "none"]);
    let low = model.variants[0].settings.as_ref().expect("the low variant settings");
    assert_eq!(
        low.rest.0.iter().find(|(key, _)| key == "reasoningEffort").map(|(_, value)| value),
        Some(&json!("low"))
    );
    assert_eq!(model.status, ModelInfoStatus::Active);

    let fresh = discovered.get("standalone").expect("the standalone model");
    assert_eq!(fresh.name, "standalone");
    assert_eq!(fresh.family, None);
    assert!(fresh.capabilities.tools);
    assert_eq!(fresh.capabilities.input, ["text"]);
    assert_eq!(fresh.capabilities.output, ["text"]);
    assert!(fresh.variants.is_empty());
    assert_eq!(fresh.limit.context, 64000.0);
    assert_eq!(fresh.limit.output, 0.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keeps_template_cost_and_limits_when_the_proxy_omits_them() {
    let sender = RecordedSender::ordered(vec![Exchange::new(
        "GET",
        &format!("{BASE}/models"),
        200,
        json!({ "data": [{ "id": "sparse", "hugging_face_id": "hf-base" }] }).to_string(),
    )]);

    let base = template(
        "hf-base",
        json!({
            "cost": [{
                "input": 5,
                "output": 10,
                "cache": { "read": 1, "write": 2 },
            }],
            "limit": { "context": 1000, "input": 500, "output": 250 },
        }),
    );
    let discovered =
        models::get(&sender, "https://modal.example", "test-key", &[base]).await.expect("the modal models");

    let model = discovered.get("sparse").expect("the sparse model");
    assert_eq!(model.name, "hf-base catalog");
    assert_eq!(model.cost[0].input, MoneyUSDPerMillionTokens(5.0));
    assert_eq!(model.cost[0].output, MoneyUSDPerMillionTokens(10.0));
    assert_eq!(model.cost[0].cache.read, MoneyUSDPerMillionTokens(1.0));
    assert_eq!(model.cost[0].cache.write, MoneyUSDPerMillionTokens(2.0));
    assert_eq!(model.limit.context, 1000.0);
    assert_eq!(model.limit.input, Some(500.0));
    assert_eq!(model.limit.output, 250.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fails_on_proxy_failure_so_the_plugin_fails_soft() {
    let url = format!("{BASE}/models");
    let sender = RecordedSender::ordered(vec![Exchange::new("GET", &url, 500, "nope")]);
    assert!(models::get(&sender, BASE, "test-key", &[]).await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovers_modal_models_through_the_plugin() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = seed_provider(&harness, BASE).await;
    harness.credential(PROVIDER_ID, credential("test-key")).await;

    let routes = Routes::new();
    routes.route_json(
        "GET",
        &format!("{BASE}/models"),
        json!({ "data": [{ "id": "live-model", "name": "Live Model" }] }),
    );
    let context = plugin_context_with(&harness, routes.clone());
    harness.register_provider_plugins(&["modal"], context).await;

    let model = wait_for_model(&harness, "live-model").await;
    assert_eq!(model.name, "Live Model");
    assert_eq!(model.package.as_deref(), Some(models::PACKAGE));
    assert_eq!(routes.count("GET", &format!("{BASE}/models")), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drops_the_previous_connection_models_after_a_credential_switch() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = seed_provider(&harness, BASE).await;
    harness.credential(PROVIDER_ID, credential("a-key")).await;

    let routes = Routes::new();
    routes.route_with("GET", &format!("{BASE}/models"), |request| {
        if authorization(request).as_deref() == Some("Bearer a-key") {
            return Reply::json(json!({ "data": [{ "id": "a-model", "name": "A" }] }));
        }
        if authorization(request).as_deref() == Some("Bearer b-key") {
            return Reply::json(json!({ "data": [{ "id": "b-model", "name": "B" }] }));
        }
        Reply::status(401)
    });
    let context = plugin_context_with(&harness, routes.clone());
    harness.register_provider_plugins(&["modal"], context).await;

    wait_for_model(&harness, "a-model").await;
    harness.credential(PROVIDER_ID, credential("b-key")).await;
    wait_for_model(&harness, "b-model").await;

    assert!(harness.services.model.get(&provider_id(), "a-model").await.unwrap_or(None).is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn falls_back_to_the_proxy_token_environment_without_a_connected_credential() {
    let mut harness = start(Input::new(Vec::new()).environment("MODAL_PROXY_TOKEN", "env-key")).await;
    let _seeded = seed_provider(&harness, BASE).await;

    let routes = Routes::new();
    routes.route_with("GET", &format!("{BASE}/models"), |request| {
        if authorization(request).as_deref() == Some("Bearer env-key") {
            return Reply::json(json!({ "data": [{ "id": "env-model", "name": "Env" }] }));
        }
        Reply::status(401)
    });
    let context = plugin_context_with(&harness, routes.clone());
    harness.register_provider_plugins(&["modal"], context).await;

    let model = wait_for_model(&harness, "env-model").await;
    assert_eq!(model.name, "Env");
}

#[test]
fn keys_the_shared_cache_by_access_credential_and_trimmed_base_url() {
    let fingerprint = opencode_catalog::shared_cache::fingerprint("secret");
    let expected_suffix = format!(":{fingerprint}:https://modal.example/v1");

    assert_eq!(
        models_key(None, "secret", "https://modal.example/v1///"),
        format!("modal:env:MODAL_PROXY_TOKEN{expected_suffix}")
    );

    let env = ConnectionInfo::Env(ConnectionEnvInfo {
        r#type: ConnectionEnvInfoType::Env,
        name: "MODAL_PROXY_TOKEN".to_owned(),
        status: None,
    });
    assert_eq!(
        models_key(Some(&env), "secret", "https://modal.example/v1/"),
        format!("modal:env:MODAL_PROXY_TOKEN{expected_suffix}")
    );

    let stored = ConnectionInfo::Credential(ConnectionCredentialInfo {
        r#type: ConnectionCredentialInfoType::Credential,
        id: CredentialID("cred_1".to_owned()),
        label: "first".to_owned(),
        method: ConnectionCredentialInfoMethod::Key,
        status: None,
    });
    assert_eq!(
        models_key(Some(&stored), "secret", "https://modal.example/v1"),
        format!("modal:credential:cred_1{expected_suffix}")
    );
}
