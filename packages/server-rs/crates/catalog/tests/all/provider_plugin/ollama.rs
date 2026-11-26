use std::sync::{Arc, Mutex};
use std::time::Duration;

use opencode_ai::HttpSender;
use opencode_ai::route::executor::PreparedHttp;
use opencode_catalog::builtin::Builtin;
use opencode_catalog::integration::Implementation;
use opencode_catalog::provider_plugin::local;
use opencode_catalog::provider_plugin::ollama;
use opencode_contract::generated::{
    IntegrationEnvMethod, IntegrationEnvMethodType, IntegrationID, IntegrationMethod, ModelInfo, ProviderID,
    ProviderInfo, ProviderInfoActivation,
};
use serde_json::{Value, json};

use crate::builtin::{Harness, Input, start};
use crate::provider_plugin::support::{Reply, Routes, plugin_context_at, plugin_context_with, settings_json};

const PROVIDER_ID: &str = "ollama";

const OPENAI_COMPATIBLE: &str = "@opencode/ai/providers/openai-compatible";

fn provider_id() -> ProviderID {
    ProviderID(PROVIDER_ID.to_owned())
}

fn integration_id() -> IntegrationID {
    IntegrationID(PROVIDER_ID.to_owned())
}

fn summary(model: &str, digest: &str, family: &str) -> Value {
    json!({
        "name": model,
        "model": model,
        "modified_at": "2026-01-01T00:00:00Z",
        "size": 1_000_000,
        "digest": digest,
        "details": {
            "format": "gguf",
            "family": family,
            "families": [family],
            "parameter_size": "8B",
            "quantization_level": "Q4_K_M",
        },
    })
}

fn show(family: &str, capabilities: &[&str], context: i64) -> Value {
    let mut model_info = serde_json::Map::new();
    model_info.insert("general.architecture".to_owned(), json!(family));
    model_info.insert(format!("{family}.context_length"), json!(context));
    json!({
        "parameters": "temperature 0.7",
        "details": {
            "parent_model": "",
            "format": "gguf",
            "family": family,
            "families": [family],
            "parameter_size": "8B",
            "quantization_level": "Q4_K_M",
        },
        "capabilities": capabilities,
        "model_info": model_info,
    })
}

fn configuration(settings: Value) -> Value {
    json!({ "providers": { "ollama": { "settings": settings } } })
}

fn authorization(request: &PreparedHttp) -> Option<String> {
    request.headers.iter().find(|(name, _)| name.eq_ignore_ascii_case("authorization")).map(|(_, value)| value.clone())
}

fn request_path(request: &PreparedHttp) -> String {
    url::Url::parse(&request.url).map(|url| url.path().to_owned()).unwrap_or_default()
}

fn request_body(request: &PreparedHttp) -> String {
    String::from_utf8_lossy(&request.body).into_owned()
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

async fn add_plugin(harness: &mut Harness, http: Arc<dyn HttpSender>, origin: &str, interval: Duration) {
    let context = plugin_context_with(harness, http);
    let mut builtin = Builtin::default();
    let options = local::Options { origin: Some(origin.to_owned()), interval, ..local::Options::default() };
    ollama::register_with(&harness.services, &context, &mut builtin, options).await;
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

async fn wait_for_context(harness: &Harness, id: &str, context: f64) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let model = harness.services.model.get(&provider_id(), id).await.unwrap_or(None);
            if model.is_some_and(|model| model.limit.context == context) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the model {id} did not reach the context {context}"))
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

async fn wait_for_requests(routes: &Routes, method: &str, url: &str, count: usize) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if routes.count(method, url) >= count {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the {method} {url} requests did not arrive"))
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ollama_is_registered_as_a_built_in_provider_plugin() {
    assert_eq!(ollama::ID, "opencode.provider.ollama");
    assert!(opencode_catalog::provider_plugin::ORDER.iter().any(|slot| slot.id() == ollama::ID));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skips_unconfigured_default_local_discovery_when_the_location_has_no_local_execution_plane() {
    let mut harness = start(Input::new(Vec::new())).await;
    let routes = Routes::new();
    let context = plugin_context_at(&harness, routes.clone(), Some("workspace"));
    let mut builtin = Builtin::default();
    ollama::register_with(&harness.services, &context, &mut builtin, local::Options::default()).await;
    harness.record(builtin);
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(harness.services.provider.get(&provider_id()).await.unwrap().is_none());
    assert!(routes.requests().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovers_local_completion_models_and_native_metadata() {
    const ORIGIN: &str = "http://127.0.0.1:18101";

    let mut harness = start(Input::new(Vec::new())).await;
    let routes = Routes::new();
    routes.route_json(
        "GET",
        &format!("{ORIGIN}/api/tags"),
        json!({
            "models": [
                summary("gemma3:4b", "gemma-digest", "gemma3"),
                summary("unknown-context", "unknown-digest", "llama"),
                summary("nomic-embed", "embed-digest", "llama"),
                summary("removed-model", "removed-digest", "llama"),
            ],
        }),
    );
    routes.route_json_for(
        "POST",
        &format!("{ORIGIN}/api/show"),
        "gemma3:4b",
        json!({
            "capabilities": ["completion", "tools", "vision"],
            "model_info": { "gemma3.context_length": 131_072 },
        }),
    );
    routes.route_json_for(
        "POST",
        &format!("{ORIGIN}/api/show"),
        "unknown-context",
        show("unknown", &["completion"], 0),
    );
    routes.route_json_for(
        "POST",
        &format!("{ORIGIN}/api/show"),
        "nomic-embed",
        show("nomic-bert", &["embedding"], 8192),
    );
    routes.route_status_for("POST", &format!("{ORIGIN}/api/show"), "removed-model", 404);
    add_plugin(&mut harness, routes.clone(), ORIGIN, Duration::from_secs(3600)).await;

    let gemma = wait_for_model(&harness, "gemma3:4b").await;
    let provider = harness.services.provider.get(&provider_id()).await.unwrap().expect("the ollama provider");
    assert_eq!(provider.name, "Ollama");
    assert_eq!(provider.activation, ProviderInfoActivation::Enabled);
    assert_eq!(provider.package, OPENAI_COMPATIBLE);
    assert_eq!(
        settings_json(&provider),
        Some(json!({ "baseURL": format!("{ORIGIN}/v1"), "provider": "ollama", "apiKey": "" }))
    );
    assert!(harness.services.provider.available().await.unwrap().iter().any(|item| item.id == provider_id()));

    assert_eq!(gemma.model_id, "gemma3:4b");
    assert_eq!(gemma.name, "gemma3:4b");
    assert_eq!(gemma.family.as_deref(), Some("gemma3"));
    assert!(gemma.capabilities.tools);
    assert_eq!(gemma.capabilities.input, ["text", "image"]);
    assert_eq!(gemma.capabilities.output, ["text"]);
    assert_eq!(gemma.limit.context, 131_072.0);
    assert_eq!(gemma.limit.output, 32_000.0);

    let unknown =
        harness.services.model.get(&provider_id(), "unknown-context").await.unwrap().expect("unknown-context");
    assert_eq!(unknown.limit.context, 200_000.0);
    assert_eq!(unknown.limit.output, 32_000.0);

    assert!(harness.services.model.get(&provider_id(), "nomic-embed").await.unwrap().is_none());

    let requests = routes.requests();
    assert!(requests.iter().any(|request| request.method == "GET" && request_path(request) == "/api/tags"));
    for id in ["gemma3:4b", "unknown-context", "nomic-embed", "removed-model"] {
        assert!(
            requests.iter().any(|request| request.method == "POST"
                && request_path(request) == "/api/show"
                && request_body(request) == format!("{{\"model\":\"{id}\"}}")),
            "the {id} show request"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refreshes_changed_digests_and_retains_inventory_through_transient_failures() {
    const ORIGIN: &str = "http://127.0.0.1:18102";

    struct State {
        digest: String,
        context: i64,
        failing: bool,
    }

    let mut harness = start(Input::new(Vec::new())).await;
    let routes = Routes::new();
    let state = Arc::new(Mutex::new(State { digest: "digest-1".to_owned(), context: 32_768, failing: false }));
    let served = Arc::new(Mutex::new(32_768i64));
    let tags = format!("{ORIGIN}/api/tags");
    let shows = format!("{ORIGIN}/api/show");
    {
        let state = state.clone();
        let served = served.clone();
        let tags = tags.clone();
        routes.route_with("GET", &tags, move |_| {
            let state = state.lock().expect("the ollama state lock is never poisoned");
            if state.failing {
                return Reply::status(503);
            }
            *served.lock().expect("the ollama served lock is never poisoned") = state.context;
            Reply::json(json!({ "models": [summary("qwen3:8b", &state.digest, "qwen3")] }))
        });
    }
    {
        let served = served.clone();
        routes.route_with("POST", &shows, move |_| {
            let context = *served.lock().expect("the ollama served lock is never poisoned");
            Reply::json(show("qwen3", &["completion", "tools"], context))
        });
    }
    add_plugin(&mut harness, routes.clone(), ORIGIN, Duration::from_millis(5)).await;

    wait_for_context(&harness, "qwen3:8b", 32_768.0).await;
    wait_for_requests(&routes, "GET", &tags, 2).await;
    assert_eq!(routes.count("POST", &shows), 1);

    {
        let mut state = state.lock().expect("the ollama state lock is never poisoned");
        state.digest = "digest-2".to_owned();
        state.context = 65_536;
    }
    wait_for_context(&harness, "qwen3:8b", 65_536.0).await;
    assert_eq!(routes.count("POST", &shows), 2);

    state.lock().expect("the ollama state lock is never poisoned").failing = true;
    tokio::time::sleep(Duration::from_millis(40)).await;
    let model = harness.services.model.get(&provider_id(), "qwen3:8b").await.unwrap().expect("qwen3:8b");
    assert_eq!(model.limit.context, 65_536.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replaces_and_restores_the_same_id_models_dev_provider() {
    const ORIGIN: &str = "http://127.0.0.1:18103";

    let mut harness = start(Input::new(Vec::new())).await;
    let routes = Routes::new();
    routes.route_json(
        "GET",
        &format!("{ORIGIN}/api/tags"),
        json!({ "models": [summary("discovered-model", "digest", "llama")] }),
    );
    routes.route_json("POST", &format!("{ORIGIN}/api/show"), show("llama", &["completion"], 32_768));
    let _integrations = harness
        .services
        .integration
        .transform(|editor| {
            let id = integration_id();
            editor.update(&id, |integration| integration.name = "Ollama".to_owned());
            editor.method_update(Implementation::Method {
                integration_id: id,
                method: IntegrationMethod::Env(IntegrationEnvMethod {
                    r#type: IntegrationEnvMethodType::Env,
                    names: vec!["OLLAMA_API_KEY".to_owned()],
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
                provider.name = "Ollama".to_owned();
                provider.package = OPENAI_COMPATIBLE.to_owned();
                provider.integration_id = Some(integration_id());
            });
            editor.models_update(&id, "static-model", |_| {});
            Ok(())
        })
        .await;

    add_plugin(&mut harness, routes.clone(), ORIGIN, Duration::from_millis(5)).await;
    wait_for_model(&harness, "discovered-model").await;

    assert!(harness.services.integration.get(&integration_id()).await.unwrap().is_none());
    assert_eq!(
        harness.services.provider.get(&provider_id()).await.unwrap().expect("the provider").activation,
        ProviderInfoActivation::Enabled
    );
    assert!(harness.services.model.get(&provider_id(), "static-model").await.unwrap().is_none());

    routes.set_json("GET", &format!("{ORIGIN}/api/tags"), json!({ "models": [] }));
    wait_for_snapshot_model(&harness, "static-model").await;

    assert!(harness.services.model.get(&provider_id(), "discovered-model").await.unwrap().is_none());
    assert!(harness.services.model.get(&provider_id(), "static-model").await.unwrap().is_none());
    assert!(harness.services.integration.get(&integration_id()).await.unwrap().is_some());
    let provider = harness.services.provider.get(&provider_id()).await.unwrap().expect("the restored provider");
    assert_eq!(provider.activation, ProviderInfoActivation::Auto);
    assert_eq!(provider.integration_id, Some(integration_id()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reloads_layered_endpoint_and_bearer_authentication_settings() {
    const INITIAL: &str = "http://127.0.0.1:18104";
    const CONFIGURED: &str = "http://127.0.0.1:18105";

    let mut harness = start(Input::new(Vec::new())).await;
    let routes = Routes::new();
    routes.route_json(
        "GET",
        &format!("{INITIAL}/api/tags"),
        json!({ "models": [summary("initial-model", "initial-digest", "llama")] }),
    );
    routes.route_json("POST", &format!("{INITIAL}/api/show"), show("llama", &["completion"], 4096));
    routes.route_json(
        "GET",
        &format!("{CONFIGURED}/proxy/api/tags"),
        json!({ "models": [summary("configured-model", "configured-digest", "llama")] }),
    );
    routes.route_json(
        "POST",
        &format!("{CONFIGURED}/proxy/api/show"),
        show("llama", &["completion", "vision"], 65_536),
    );
    add_plugin(&mut harness, routes.clone(), INITIAL, Duration::from_secs(3600)).await;
    wait_for_model(&harness, "initial-model").await;

    let base_url = format!("{CONFIGURED}/proxy/v1");
    write_entries(
        &harness,
        &[configuration(json!({ "baseURL": base_url, "apiKey": "old" })), configuration(json!({ "apiKey": "secret" }))],
    );
    harness.services.config.reload().await;
    wait_for_model(&harness, "configured-model").await;

    assert!(routes.requests().iter().any(|request| {
        authorization(request).as_deref() == Some("Bearer secret")
            && request.method == "GET"
            && request_path(request) == "/proxy/api/tags"
    }));
    assert!(routes.requests().iter().any(|request| {
        authorization(request).as_deref() == Some("Bearer secret")
            && request.method == "POST"
            && request_path(request) == "/proxy/api/show"
    }));
    assert!(harness.services.model.get(&provider_id(), "initial-model").await.unwrap().is_none());
    assert_eq!(
        settings_json(&wait_for_provider_setting(&harness, "baseURL", &base_url).await),
        Some(json!({ "baseURL": base_url, "provider": "ollama", "apiKey": "secret" }))
    );

    let mark = routes.requests().len();
    write_entries(
        &harness,
        &[configuration(json!({ "baseURL": base_url, "apiKey": "secret" })), configuration(json!({ "apiKey": null }))],
    );
    harness.services.config.reload().await;
    wait_for_provider_setting(&harness, "apiKey", "").await;
    assert!(
        routes.requests()[mark..]
            .iter()
            .any(|request| authorization(request).is_none() && request_path(request) == "/proxy/api/tags")
    );
    assert!(
        routes.requests()[mark..]
            .iter()
            .any(|request| authorization(request).is_none() && request_path(request) == "/proxy/api/show")
    );

    let mark = routes.requests().len();
    let api_base_url = format!("{CONFIGURED}/proxy/api");
    write_entries(&harness, &[configuration(json!({ "baseURL": api_base_url, "apiKey": "api-secret" }))]);
    harness.services.config.reload().await;
    wait_for_provider_setting(&harness, "baseURL", &api_base_url).await;
    assert!(routes.requests()[mark..].iter().any(|request| {
        authorization(request).as_deref() == Some("Bearer api-secret")
            && request.method == "GET"
            && request_path(request) == "/proxy/api/tags"
    }));
    assert!(routes.requests()[mark..].iter().any(|request| {
        authorization(request).as_deref() == Some("Bearer api-secret")
            && request.method == "POST"
            && request_path(request) == "/proxy/api/show"
    }));
}
