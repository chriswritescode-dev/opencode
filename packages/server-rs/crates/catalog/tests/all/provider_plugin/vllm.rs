use std::sync::{Arc, Mutex};
use std::time::Duration;

use opencode_ai::HttpSender;
use opencode_ai::route::executor::PreparedHttp;
use opencode_catalog::builtin::Builtin;
use opencode_catalog::integration::Implementation;
use opencode_catalog::provider_plugin::local;
use opencode_catalog::provider_plugin::vllm;
use opencode_contract::generated::{
    IntegrationEnvMethod, IntegrationEnvMethodType, IntegrationID, IntegrationMethod, ModelInfo, ProviderID,
    ProviderInfo, ProviderInfoActivation,
};
use serde_json::{Value, json};

use crate::builtin::{Harness, Input, start};
use crate::provider_plugin::support::{Reply, Routes, plugin_context_with, settings_json};

const PROVIDER_ID: &str = "vllm";

const OPENAI_COMPATIBLE: &str = "@opencode/ai/providers/openai-compatible";

fn provider_id() -> ProviderID {
    ProviderID(PROVIDER_ID.to_owned())
}

fn integration_id() -> IntegrationID {
    IntegrationID(PROVIDER_ID.to_owned())
}

fn remote_model(id: &str, max_model_len: i64, owned_by: &str) -> Value {
    json!({
        "id": id,
        "object": "model",
        "created": 1,
        "owned_by": owned_by,
        "root": id,
        "parent": null,
        "max_model_len": max_model_len,
        "permission": [],
    })
}

fn response(models: &[Value]) -> Value {
    json!({ "object": "list", "data": models })
}

fn configuration(settings: Value) -> Value {
    json!({ "providers": { "vllm": { "settings": settings } } })
}

fn authorization(request: &PreparedHttp) -> Option<String> {
    request.headers.iter().find(|(name, _)| name.eq_ignore_ascii_case("authorization")).map(|(_, value)| value.clone())
}

fn request_path(request: &PreparedHttp) -> String {
    url::Url::parse(&request.url).map(|url| url.path().to_owned()).unwrap_or_default()
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
    vllm::register_with(&harness.services, &context, &mut builtin, options).await;
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
async fn vllm_is_registered_as_a_built_in_provider_plugin() {
    assert_eq!(vllm::ID, "opencode.provider.vllm");
    assert!(opencode_catalog::provider_plugin::ORDER.iter().any(|slot| slot.id() == vllm::ID));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn waits_for_readiness_and_discovers_official_vllm_model_metadata() {
    const ORIGIN: &str = "http://127.0.0.1:18201";

    let mut harness = start(Input::new(Vec::new())).await;
    let routes = Routes::new();
    let healthy = Arc::new(Mutex::new(false));
    {
        let healthy = healthy.clone();
        routes.route_with("GET", &format!("{ORIGIN}/health"), move |_| {
            if *healthy.lock().expect("the vllm health lock is never poisoned") {
                Reply::empty()
            } else {
                Reply::status(503)
            }
        });
    }
    routes.route_json(
        "GET",
        &format!("{ORIGIN}/v1/models"),
        response(&[
            remote_model("Qwen/Qwen3-Coder", 65_536, "vllm"),
            remote_model("unknown-limit", 0, "vllm"),
            remote_model("foreign-model", 4096, "other"),
        ]),
    );
    add_plugin(&mut harness, routes.clone(), ORIGIN, Duration::from_millis(5)).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(harness.services.provider.get(&provider_id()).await.unwrap().is_none());
    assert_eq!(routes.count("GET", &format!("{ORIGIN}/v1/models")), 0);

    *healthy.lock().expect("the vllm health lock is never poisoned") = true;
    let model = wait_for_model(&harness, "Qwen/Qwen3-Coder").await;
    let provider = harness.services.provider.get(&provider_id()).await.unwrap().expect("the vllm provider");
    assert_eq!(provider.name, "vLLM");
    assert_eq!(provider.activation, ProviderInfoActivation::Enabled);
    assert_eq!(provider.package, OPENAI_COMPATIBLE);
    assert_eq!(
        settings_json(&provider),
        Some(json!({ "baseURL": format!("{ORIGIN}/v1"), "provider": "vllm", "apiKey": "" }))
    );
    assert!(harness.services.provider.available().await.unwrap().iter().any(|item| item.id == provider_id()));

    assert_eq!(model.model_id, "Qwen/Qwen3-Coder");
    assert_eq!(model.name, "Qwen/Qwen3-Coder");
    assert!(!model.capabilities.tools);
    assert_eq!(model.capabilities.input, ["text"]);
    assert_eq!(model.capabilities.output, ["text"]);
    assert_eq!(model.limit.context, 65_536.0);
    assert_eq!(model.limit.output, 32_000.0);

    let unknown = harness.services.model.get(&provider_id(), "unknown-limit").await.unwrap().expect("unknown-limit");
    assert_eq!(unknown.limit.context, 200_000.0);
    assert_eq!(unknown.limit.output, 32_000.0);

    assert!(harness.services.model.get(&provider_id(), "foreign-model").await.unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn waits_for_a_slow_endpoint_instead_of_dropping_its_models() {
    const ORIGIN: &str = "http://127.0.0.1:18202";

    let mut harness = start(Input::new(Vec::new())).await;
    let routes = Routes::new();
    routes.route_with("GET", &format!("{ORIGIN}/health"), |_| Reply::empty());
    routes.route_json("GET", &format!("{ORIGIN}/v1/models"), response(&[remote_model("slow-model", 32_768, "vllm")]));
    routes.hold("GET", &format!("{ORIGIN}/v1/models"));
    add_plugin(&mut harness, routes.clone(), ORIGIN, Duration::from_secs(3600)).await;

    routes.arrived().await;
    routes.release();
    wait_for_model(&harness, "slow-model").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn does_not_report_a_replaced_endpoints_failure_against_the_new_one() {
    const SLOW: &str = "http://127.0.0.1:18203";
    const HEALTHY: &str = "http://127.0.0.1:18204";

    let mut harness = start(Input::new(vec![configuration(json!({ "baseURL": format!("{SLOW}/v1") }))])).await;
    let routes = Routes::new();
    routes.route_with("GET", &format!("{SLOW}/health"), |_| Reply::empty());
    routes.hold("GET", &format!("{SLOW}/health"));
    routes.route_with("GET", &format!("{HEALTHY}/health"), |_| Reply::empty());
    routes.route_json(
        "GET",
        &format!("{HEALTHY}/v1/models"),
        response(&[remote_model("healthy-model", 32_768, "vllm")]),
    );

    let context = plugin_context_with(&harness, routes.clone());
    let mut builtin = Builtin::default();
    let options = local::Options {
        origin: Some(SLOW.to_owned()),
        interval: Duration::from_secs(3600),
        timeout: Duration::from_millis(150),
    };
    vllm::register_with(&harness.services, &context, &mut builtin, options).await;
    harness.record(builtin);

    routes.arrived().await;
    let base_url = format!("{HEALTHY}/v1");
    write_entries(&harness, &[configuration(json!({ "baseURL": base_url }))]);
    harness.services.config.reload().await;
    wait_for_model(&harness, "healthy-model").await;

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        settings_json(&wait_for_provider_setting(&harness, "baseURL", &base_url).await),
        Some(json!({ "baseURL": base_url, "provider": "vllm", "apiKey": "" }))
    );
    assert!(harness.services.model.get(&provider_id(), "healthy-model").await.unwrap().is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refreshes_inventory_while_retaining_the_last_success_through_transient_failures() {
    const ORIGIN: &str = "http://127.0.0.1:18205";

    struct State {
        failing: bool,
        models: Vec<Value>,
    }

    let mut harness = start(Input::new(Vec::new())).await;
    let routes = Routes::new();
    let state =
        Arc::new(Mutex::new(State { failing: false, models: vec![remote_model("first-model", 32_768, "vllm")] }));
    {
        let state = state.clone();
        routes.route_with("GET", &format!("{ORIGIN}/health"), move |_| {
            if state.lock().expect("the vllm state lock is never poisoned").failing {
                return Reply::status(503);
            }
            Reply::empty()
        });
    }
    {
        let state = state.clone();
        routes.route_with("GET", &format!("{ORIGIN}/v1/models"), move |_| {
            let state = state.lock().expect("the vllm state lock is never poisoned");
            if state.failing {
                return Reply::status(503);
            }
            Reply::json(response(&state.models))
        });
    }
    add_plugin(&mut harness, routes.clone(), ORIGIN, Duration::from_millis(5)).await;
    wait_for_model(&harness, "first-model").await;

    {
        let mut state = state.lock().expect("the vllm state lock is never poisoned");
        state.failing = true;
        state.models = vec![remote_model("second-model", 32_768, "vllm")];
    }
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert!(harness.services.model.get(&provider_id(), "first-model").await.unwrap().is_some());
    assert!(harness.services.model.get(&provider_id(), "second-model").await.unwrap().is_none());

    state.lock().expect("the vllm state lock is never poisoned").failing = false;
    wait_for_model(&harness, "second-model").await;
    assert!(harness.services.model.get(&provider_id(), "first-model").await.unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replaces_and_restores_same_id_models_dev_entries_after_an_empty_success() {
    const ORIGIN: &str = "http://127.0.0.1:18206";

    let mut harness = start(Input::new(Vec::new())).await;
    let routes = Routes::new();
    let payload = Arc::new(Mutex::new(vec![remote_model("discovered-model", 32_768, "vllm")]));
    {
        let payload = payload.clone();
        routes.route_with("GET", &format!("{ORIGIN}/v1/models"), move |_| {
            Reply::json(response(&payload.lock().expect("the vllm models lock is never poisoned")))
        });
    }
    routes.route_with("GET", &format!("{ORIGIN}/health"), |_| Reply::empty());
    let _integrations = harness
        .services
        .integration
        .transform(|editor| {
            let id = integration_id();
            editor.update(&id, |integration| integration.name = "vLLM".to_owned());
            editor.method_update(Implementation::Method {
                integration_id: id,
                method: IntegrationMethod::Env(IntegrationEnvMethod {
                    r#type: IntegrationEnvMethodType::Env,
                    names: vec!["VLLM_API_KEY".to_owned()],
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
                provider.name = "vLLM".to_owned();
                provider.package = OPENAI_COMPATIBLE.to_owned();
                provider.integration_id = Some(integration_id());
                provider.activation = ProviderInfoActivation::Auto;
            });
            editor.models_update(&id, "static-model", |_| {});
            Ok(())
        })
        .await;

    add_plugin(&mut harness, routes.clone(), ORIGIN, Duration::from_millis(5)).await;
    wait_for_model(&harness, "discovered-model").await;

    assert!(harness.services.integration.get(&integration_id()).await.unwrap().is_none());
    let provider = harness.services.provider.get(&provider_id()).await.unwrap().expect("the vllm provider");
    assert_eq!(provider.integration_id, None);
    assert_eq!(provider.activation, ProviderInfoActivation::Enabled);
    assert!(harness.services.model.get(&provider_id(), "static-model").await.unwrap().is_none());

    payload.lock().expect("the vllm models lock is never poisoned").clear();
    wait_for_snapshot_model(&harness, "static-model").await;

    assert!(harness.services.model.get(&provider_id(), "discovered-model").await.unwrap().is_none());
    assert!(harness.services.model.get(&provider_id(), "static-model").await.unwrap().is_none());
    assert!(harness.services.integration.get(&integration_id()).await.unwrap().is_some());
    let provider = harness.services.provider.get(&provider_id()).await.unwrap().expect("the restored provider");
    assert_eq!(provider.integration_id, Some(integration_id()));
    assert_eq!(provider.activation, ProviderInfoActivation::Auto);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reloads_layered_custom_endpoint_and_bearer_authentication_settings() {
    const INITIAL: &str = "http://127.0.0.1:18207";
    const CONFIGURED: &str = "http://127.0.0.1:18208";

    let mut harness = start(Input::new(Vec::new())).await;
    let routes = Routes::new();
    routes.route_with("GET", &format!("{INITIAL}/health"), |_| Reply::empty());
    routes.route_json(
        "GET",
        &format!("{INITIAL}/v1/models"),
        response(&[remote_model("initial-model", 32_768, "vllm")]),
    );
    routes.route_with("GET", &format!("{CONFIGURED}/proxy/health"), |_| Reply::empty());
    routes.route_json(
        "GET",
        &format!("{CONFIGURED}/proxy/v1/models"),
        response(&[remote_model("configured-model", 32_768, "vllm")]),
    );
    add_plugin(&mut harness, routes.clone(), INITIAL, Duration::from_secs(3600)).await;
    wait_for_model(&harness, "initial-model").await;

    let mark = routes.requests().len();
    let base_url = format!("{CONFIGURED}/proxy/v1");
    write_entries(
        &harness,
        &[configuration(json!({ "baseURL": base_url })), configuration(json!({ "apiKey": "secret" }))],
    );
    harness.services.config.reload().await;
    wait_for_model(&harness, "configured-model").await;

    assert!(routes.requests()[mark..].iter().any(|request| authorization(request).as_deref() == Some("Bearer secret")
        && request_path(request) == "/proxy/health"));
    assert!(routes.requests()[mark..].iter().any(|request| authorization(request).as_deref() == Some("Bearer secret")
        && request_path(request) == "/proxy/v1/models"));
    assert!(harness.services.model.get(&provider_id(), "initial-model").await.unwrap().is_none());
    assert_eq!(
        settings_json(&wait_for_provider_setting(&harness, "apiKey", "secret").await),
        Some(json!({ "baseURL": base_url, "provider": "vllm", "apiKey": "secret" }))
    );

    let mark = routes.requests().len();
    write_entries(
        &harness,
        &[configuration(json!({ "baseURL": base_url })), configuration(json!({ "apiKey": "next-secret" }))],
    );
    harness.services.config.reload().await;
    wait_for_provider_setting(&harness, "apiKey", "next-secret").await;
    assert!(
        routes.requests()[mark..].iter().any(|request| authorization(request).as_deref() == Some("Bearer next-secret")
            && request_path(request) == "/proxy/health")
    );
    assert!(
        routes.requests()[mark..].iter().any(|request| authorization(request).as_deref() == Some("Bearer next-secret")
            && request_path(request) == "/proxy/v1/models")
    );
}
