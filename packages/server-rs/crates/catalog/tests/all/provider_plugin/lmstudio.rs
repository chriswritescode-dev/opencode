use std::sync::{Arc, Mutex};
use std::time::Duration;

use opencode_ai::HttpSender;
use opencode_ai::route::executor::PreparedHttp;
use opencode_catalog::builtin::Builtin;
use opencode_catalog::integration::Implementation;
use opencode_catalog::provider_plugin::lmstudio;
use opencode_catalog::provider_plugin::local;
use opencode_contract::generated::{
    CredentialKey, CredentialKeyType, CredentialValue, IntegrationEnvMethod, IntegrationEnvMethodType, IntegrationID,
    IntegrationKeyMethod, IntegrationKeyMethodType, IntegrationMethod, ModelInfo, ModelType, ModelTypeString2,
    ProviderID, ProviderInfo, ProviderInfoActivation,
};
use serde_json::{Value, json};

use crate::builtin::{Harness, Input, start};
use crate::provider_plugin::support::{Reply, Routes, plugin_context_with, settings_json};

const PROVIDER_ID: &str = "lmstudio";

const OPENAI_COMPATIBLE: &str = "@opencode/ai/providers/openai-compatible";

fn provider_id() -> ProviderID {
    ProviderID(PROVIDER_ID.to_owned())
}

fn integration_id() -> IntegrationID {
    IntegrationID(PROVIDER_ID.to_owned())
}

fn models(items: &[Value]) -> Value {
    json!({ "models": items })
}

fn model(key: &str) -> Value {
    json!({
        "type": "llm",
        "key": key,
        "display_name": key,
        "loaded_instances": [],
        "max_context_length": 32_768,
    })
}

fn authorization(request: &PreparedHttp) -> Option<String> {
    request.headers.iter().find(|(name, _)| name.eq_ignore_ascii_case("authorization")).map(|(_, value)| value.clone())
}

fn request_path(request: &PreparedHttp) -> String {
    url::Url::parse(&request.url).map(|url| url.path().to_owned()).unwrap_or_default()
}

async fn add_plugin(harness: &mut Harness, http: Arc<dyn HttpSender>, origin: &str, interval: Duration) {
    let context = plugin_context_with(harness, http);
    let mut builtin = Builtin::default();
    let options = local::Options { origin: Some(origin.to_owned()), interval, ..local::Options::default() };
    lmstudio::register_with(&harness.services, &context, &mut builtin, options).await;
    harness.record(builtin);
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

async fn wait_for_provider(harness: &Harness) -> ProviderInfo {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(provider) = harness.services.provider.get(&provider_id()).await.unwrap_or(None) {
                return provider;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("the lmstudio provider appears")
}

async fn wait_for_provider_setting(harness: &Harness, key: &str, value: &str) -> ProviderInfo {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(provider) = harness.services.provider.get(&provider_id()).await.unwrap_or(None)
                && settings_json(&provider)
                    .and_then(|settings| settings.get(key).and_then(Value::as_str).map(|current| current == value))
                    .unwrap_or(false)
            {
                return provider;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the provider setting {key} did not settle"))
}

async fn wait_for_requests(routes: &Routes, count: usize) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if routes.requests().len() >= count {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the discovery requests did not arrive"))
}

async fn wait_for_snapshot_model(harness: &Harness, model_id: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = harness.services.provider.snapshot().await.expect("the provider snapshot");
            if snapshot.records.get(PROVIDER_ID).is_some_and(|definition| definition.models.contains_key(model_id)) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the static model {model_id} did not return"))
}

fn write_entries(harness: &Harness, entries: &[Value]) {
    let global = entries.first().cloned().unwrap_or_else(|| json!({}));
    let project = entries.get(1).cloned().unwrap_or_else(|| json!({}));
    std::fs::write(
        harness.root().join("global").join("opencode.json"),
        serde_json::to_string(&global).expect("the global config is json"),
    )
    .expect("write the global config");
    std::fs::write(
        harness.root().join("project").join("opencode.json"),
        serde_json::to_string(&project).expect("the project config is json"),
    )
    .expect("write the project config");
}

fn configuration(base_url: &str, api_key: Option<&str>) -> Value {
    json!({
        "providers": {
            "lmstudio": {
                "settings": { "baseURL": base_url, "apiKey": api_key }
            }
        }
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lmstudio_is_registered_as_a_built_in_provider_plugin() {
    assert_eq!(lmstudio::ID, "opencode.provider.lmstudio");
    assert!(opencode_catalog::provider_plugin::ORDER.iter().any(|slot| slot.id() == lmstudio::ID));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovers_local_language_models_with_their_capabilities_and_effective_context() {
    const ORIGIN: &str = "http://127.0.0.1:18001";

    let mut harness = start(Input::new(Vec::new())).await;
    let routes = Routes::new();
    routes.route_json(
        "GET",
        &format!("{ORIGIN}/api/v1/models"),
        models(&[
            json!({
                "type": "llm",
                "key": "google/gemma-4-26b-a4b",
                "display_name": "Gemma 4 26B A4B",
                "architecture": "gemma4",
                "loaded_instances": [
                    { "config": { "context_length": 32_768 } },
                    { "config": { "context_length": 16_384 } },
                ],
                "max_context_length": 262_144,
                "capabilities": { "vision": true, "trained_for_tool_use": true },
            }),
            json!({
                "type": "llm",
                "key": "deepseek-r1",
                "display_name": "DeepSeek R1",
                "architecture": "deepseek",
                "loaded_instances": [],
                "max_context_length": 131_072,
                "capabilities": { "vision": false, "trained_for_tool_use": false },
            }),
            json!({
                "type": "llm",
                "key": "unknown-context",
                "display_name": "Unknown Context",
                "loaded_instances": [],
                "max_context_length": 0,
            }),
            json!({
                "type": "embedding",
                "key": "nomic-embed",
                "display_name": "Nomic Embed",
                "loaded_instances": [],
                "max_context_length": 2048,
            }),
            json!({
                "type": "decision",
                "key": "clef-flash",
                "display_name": "Clef Flash",
                "loaded_instances": [],
                "max_context_length": null,
                "has_vision_adapter": true,
            }),
        ]),
    );
    add_plugin(&mut harness, routes.clone(), ORIGIN, Duration::from_secs(3600)).await;

    let gemma = wait_for_model(&harness, "google/gemma-4-26b-a4b").await;
    let provider = harness.services.provider.get(&provider_id()).await.unwrap().expect("the lmstudio provider");
    assert_eq!(provider.name, "LM Studio");
    assert_eq!(provider.activation, ProviderInfoActivation::Enabled);
    assert_eq!(provider.package, OPENAI_COMPATIBLE);
    assert_eq!(
        settings_json(&provider),
        Some(json!({ "baseURL": format!("{ORIGIN}/v1"), "provider": "lmstudio", "apiKey": "" }))
    );
    assert!(harness.services.provider.available().await.unwrap().iter().any(|item| item.id == provider_id()));

    assert_eq!(gemma.family.as_deref(), Some("gemma4"));
    assert_eq!(gemma.name, "Gemma 4 26B A4B");
    assert!(gemma.capabilities.tools);
    assert_eq!(gemma.capabilities.input, ["text", "image"]);
    assert_eq!(gemma.capabilities.output, ["text"]);
    assert_eq!(gemma.limit.context, 16_384.0);
    assert_eq!(gemma.limit.output, 32_000.0);

    let deepseek = harness.services.model.get(&provider_id(), "deepseek-r1").await.unwrap().expect("deepseek-r1");
    assert!(!deepseek.capabilities.tools);
    assert_eq!(deepseek.capabilities.input, ["text"]);
    assert_eq!(deepseek.capabilities.output, ["text"]);
    assert_eq!(deepseek.limit.context, 131_072.0);
    assert_eq!(deepseek.limit.output, 32_000.0);

    let unknown = harness.services.model.get(&provider_id(), "unknown-context").await.unwrap().expect("unknown");
    assert_eq!(unknown.limit.context, 200_000.0);
    assert_eq!(unknown.limit.output, 32_000.0);

    assert!(harness.services.model.get(&provider_id(), "nomic-embed").await.unwrap().is_none());
    assert!(harness.services.model.get(&provider_id(), "clef-flash").await.unwrap().is_none());

    let clef = harness
        .services
        .model
        .available(Some(&[ModelType::String2(ModelTypeString2::Decision)]))
        .await
        .unwrap()
        .into_iter()
        .find(|model| model.provider_id == provider_id())
        .expect("the decision model");
    assert_eq!(clef.id, "clef-flash");
    assert_eq!(clef.r#type, Some(ModelType::String2(ModelTypeString2::Decision)));
    assert_eq!(clef.name, "Clef Flash");
    assert_eq!(clef.package.as_deref(), Some("@opencode/ai/providers/openai-compatible/decisions"));
    assert!(!clef.capabilities.tools);
    assert_eq!(clef.capabilities.input, ["text", "image"]);
    assert_eq!(clef.capabilities.output, ["decision"]);
    assert_eq!(clef.limit.context, 200_000.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refreshes_the_catalog_when_lm_studio_models_change() {
    const ORIGIN: &str = "http://127.0.0.1:18002";

    let mut harness = start(Input::new(Vec::new())).await;
    let routes = Routes::new();
    let payload = Arc::new(Mutex::new(models(&[])));
    let current = payload.clone();
    routes.route_with("GET", &format!("{ORIGIN}/api/v1/models"), move |_| {
        Reply::json(current.lock().expect("the lmstudio models lock is never poisoned").clone())
    });
    add_plugin(&mut harness, routes.clone(), ORIGIN, Duration::from_millis(5)).await;
    assert!(harness.services.provider.get(&provider_id()).await.unwrap().is_none());

    *payload.lock().expect("the lmstudio models lock is never poisoned") = models(&[json!({
        "type": "llm",
        "key": "qwen/qwen3-coder",
        "display_name": "Qwen 3 Coder",
        "architecture": "qwen3",
        "loaded_instances": [],
        "max_context_length": 65_536,
        "capabilities": { "vision": false, "trained_for_tool_use": true },
    })]);
    let coder = wait_for_model(&harness, "qwen/qwen3-coder").await;
    assert_eq!(coder.name, "Qwen 3 Coder");

    *payload.lock().expect("the lmstudio models lock is never poisoned") = models(&[]);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if harness.services.provider.get(&provider_id()).await.unwrap().is_none() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("the lmstudio provider disappears");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovers_from_configured_endpoints_with_bearer_authentication() {
    const INITIAL: &str = "http://127.0.0.1:18003";
    const CONFIGURED: &str = "http://127.0.0.1:18004";

    let mut harness = start(Input::new(Vec::new())).await;
    let routes = Routes::new();
    routes.route_json("GET", &format!("{INITIAL}/api/v1/models"), models(&[model("initial-model")]));
    routes.route_json("GET", &format!("{CONFIGURED}/proxy/api/v1/models"), models(&[model("configured-model")]));
    add_plugin(&mut harness, routes.clone(), INITIAL, Duration::from_secs(3600)).await;
    wait_for_model(&harness, "initial-model").await;

    let base_url = format!("{CONFIGURED}/proxy/v1");
    write_entries(&harness, &[configuration(&base_url, Some("secret"))]);
    harness.services.config.reload().await;
    wait_for_model(&harness, "configured-model").await;

    assert!(routes.requests().iter().any(|request| {
        authorization(request).as_deref() == Some("Bearer secret") && request_path(request) == "/proxy/api/v1/models"
    }));
    assert!(harness.services.model.get(&provider_id(), "initial-model").await.unwrap().is_none());
    assert_eq!(
        settings_json(&wait_for_provider(&harness).await),
        Some(json!({ "baseURL": base_url, "provider": "lmstudio", "apiKey": "secret" }))
    );

    let mark = routes.requests().len();
    write_entries(&harness, &[configuration(&base_url, Some("secret")), configuration(&base_url, None)]);
    harness.services.config.reload().await;
    wait_for_provider_setting(&harness, "apiKey", "").await;
    assert!(
        routes.requests()[mark..]
            .iter()
            .any(|request| authorization(request).is_none() && request_path(request) == "/proxy/api/v1/models")
    );

    let mark = routes.requests().len();
    let api_base_url = format!("{CONFIGURED}/proxy/api/v1");
    write_entries(&harness, &[configuration(&api_base_url, Some("api-secret"))]);
    harness.services.config.reload().await;
    wait_for_provider_setting(&harness, "baseURL", &api_base_url).await;
    assert!(
        routes.requests()[mark..].iter().any(|request| authorization(request).as_deref() == Some("Bearer api-secret")
            && request_path(request) == "/proxy/api/v1/models")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovers_with_a_key_saved_through_connect_as_soon_as_it_is_saved() {
    const ORIGIN: &str = "http://127.0.0.1:18005";

    let mut harness = start(Input::new(Vec::new())).await;
    let routes = Routes::new();
    routes.route_with("GET", &format!("{ORIGIN}/api/v1/models"), |request| {
        if authorization(request).as_deref() == Some("Bearer saved-key") {
            return Reply::json(models(&[model("secured-model")]));
        }
        Reply::json_status(json!({ "error": { "code": "invalid_api_key" } }), 401)
    });
    let _seeded = harness
        .services
        .integration
        .transform(|editor| {
            let id = integration_id();
            editor.update(&id, |integration| integration.name = "LMStudio".to_owned());
            editor.method_update(Implementation::Method {
                integration_id: id,
                method: IntegrationMethod::Key(IntegrationKeyMethod {
                    r#type: IntegrationKeyMethodType::Key,
                    label: None,
                    form: None,
                }),
            });
            Ok(())
        })
        .await;
    add_plugin(&mut harness, routes.clone(), ORIGIN, Duration::from_secs(3600)).await;
    wait_for_requests(&routes, 1).await;
    assert_eq!(routes.requests().len(), 1);
    assert!(authorization(&routes.requests()[0]).is_none());

    harness
        .credential(
            PROVIDER_ID,
            CredentialValue::Key(CredentialKey {
                r#type: CredentialKeyType::Key,
                key: "saved-key".to_owned(),
                metadata: None,
            }),
        )
        .await;
    wait_for_model(&harness, "secured-model").await;
    assert_eq!(
        authorization(routes.requests().last().expect("a discovery request")).as_deref(),
        Some("Bearer saved-key")
    );
    assert!(harness.services.integration.get(&integration_id()).await.unwrap().is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keeps_sending_the_integrations_env_key_after_models_load() {
    const ORIGIN: &str = "http://127.0.0.1:18006";

    let mut harness = start(Input::new(Vec::new()).environment("OPENCODE_TEST_LMSTUDIO_KEY", "env-key")).await;
    let routes = Routes::new();
    routes.route_with("GET", &format!("{ORIGIN}/api/v1/models"), |request| {
        if authorization(request).as_deref() == Some("Bearer env-key") {
            return Reply::json(models(&[model("secured-model")]));
        }
        Reply::json_status(json!({ "error": { "code": "invalid_api_key" } }), 401)
    });
    let _seeded = harness
        .services
        .integration
        .transform(|editor| {
            let id = integration_id();
            editor.update(&id, |integration| integration.name = "LMStudio".to_owned());
            editor.method_update(Implementation::Method {
                integration_id: id,
                method: IntegrationMethod::Env(IntegrationEnvMethod {
                    r#type: IntegrationEnvMethodType::Env,
                    names: vec!["OPENCODE_TEST_LMSTUDIO_KEY".to_owned()],
                }),
            });
            Ok(())
        })
        .await;
    add_plugin(&mut harness, routes.clone(), ORIGIN, Duration::from_millis(5)).await;
    wait_for_model(&harness, "secured-model").await;
    wait_for_requests(&routes, 3).await;

    assert!(routes.requests().iter().all(|request| authorization(request).as_deref() == Some("Bearer env-key")));
    assert!(harness.services.integration.get(&integration_id()).await.unwrap().is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shares_discovery_requests_across_plugin_instances() {
    const ORIGIN: &str = "http://127.0.0.1:18007";

    let mut harness = start(Input::new(Vec::new())).await;
    let routes = Routes::new();
    routes.route_json("GET", &format!("{ORIGIN}/api/v1/models"), models(&[model("shared-model")]));
    add_plugin(&mut harness, routes.clone(), ORIGIN, Duration::from_secs(3600)).await;
    add_plugin(&mut harness, routes.clone(), ORIGIN, Duration::from_secs(3600)).await;
    wait_for_model(&harness, "shared-model").await;
    assert_eq!(routes.count("GET", &format!("{ORIGIN}/api/v1/models")), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replaces_the_credential_gated_models_dev_catalog_when_discovery_succeeds() {
    const ORIGIN: &str = "http://127.0.0.1:18008";

    let mut harness = start(Input::new(Vec::new())).await;
    let routes = Routes::new();
    let payload = Arc::new(Mutex::new(models(&[model("discovered-model")])));
    let current = payload.clone();
    routes.route_with("GET", &format!("{ORIGIN}/api/v1/models"), move |_| {
        Reply::json(current.lock().expect("the lmstudio models lock is never poisoned").clone())
    });
    let _integrations = harness
        .services
        .integration
        .transform(|editor| {
            let id = integration_id();
            editor.update(&id, |integration| integration.name = "LMStudio".to_owned());
            editor.method_update(Implementation::Method {
                integration_id: id,
                method: IntegrationMethod::Env(IntegrationEnvMethod {
                    r#type: IntegrationEnvMethodType::Env,
                    names: vec!["LMSTUDIO_API_KEY".to_owned()],
                }),
            });
            Ok(())
        })
        .await;
    let _providers = harness
        .services
        .provider
        .transform(|editor| {
            let id = provider_id();
            editor.update(&id, |provider| {
                provider.name = "LMStudio".to_owned();
                provider.package = OPENAI_COMPATIBLE.to_owned();
                provider.integration_id = Some(integration_id());
            });
            editor.models_update(&id, "static-model", |_| {});
            Ok(())
        })
        .await;

    assert!(!harness.services.provider.available().await.unwrap().iter().any(|item| item.id == provider_id()));
    add_plugin(&mut harness, routes.clone(), ORIGIN, Duration::from_millis(5)).await;
    wait_for_model(&harness, "discovered-model").await;

    assert!(harness.services.integration.get(&integration_id()).await.unwrap().is_none());
    assert_eq!(
        harness.services.provider.get(&provider_id()).await.unwrap().expect("the provider").integration_id,
        None
    );
    assert!(harness.services.model.get(&provider_id(), "static-model").await.unwrap().is_none());
    assert!(harness.services.provider.available().await.unwrap().iter().any(|item| item.id == provider_id()));

    let _key = harness
        .services
        .integration
        .transform(|editor| {
            let id = integration_id();
            editor.update(&id, |integration| integration.name = "Configured LM Studio".to_owned());
            editor.method_update(Implementation::Method {
                integration_id: id,
                method: IntegrationMethod::Key(IntegrationKeyMethod {
                    r#type: IntegrationKeyMethodType::Key,
                    label: None,
                    form: None,
                }),
            });
            Ok(())
        })
        .await;
    assert!(harness.services.provider.available().await.unwrap().iter().any(|item| item.id == provider_id()));

    *payload.lock().expect("the lmstudio models lock is never poisoned") = models(&[]);
    wait_for_snapshot_model(&harness, "static-model").await;
    assert!(harness.services.model.get(&provider_id(), "discovered-model").await.unwrap().is_none());
    assert!(harness.services.model.get(&provider_id(), "static-model").await.unwrap().is_none());
    assert!(harness.services.integration.get(&integration_id()).await.unwrap().is_some());
    assert_eq!(
        harness.services.provider.get(&provider_id()).await.unwrap().expect("the provider").integration_id,
        Some(integration_id())
    );
}
