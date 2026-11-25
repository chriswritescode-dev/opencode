use std::ffi::OsString;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use indexmap::IndexMap;
use opencode_ai::route::executor::PreparedHttp;
use opencode_bus::{Definition, PublishOptions};
use opencode_catalog::builtin::Builtin;
use opencode_catalog::credential::{CreateInput, Update};
use opencode_catalog::integration::ExternalInput;
use opencode_catalog::provider_plugin::azure::discovery::Endpoints;
use opencode_catalog::provider_plugin::azure::{self, Options};
use opencode_catalog::provider_plugin::hooks::{HttpRequest, ProviderHooks, RequestKind, WsHandshake};
use opencode_catalog::provider_plugin::{BoxFuture, CommandOutput, CommandRunner};
use opencode_contract::codec::{JsonMap, decode_json};
use opencode_contract::generated::{
    AgentID, CredentialExternal, CredentialExternalType, CredentialID, CredentialKey, CredentialKeyType,
    CredentialValue, FormField1, FormStringField1Type, FormValue, IntegrationID, IntegrationMethod,
    IntegrationMethodID, ModelInfo, ModelInfoLimit, ModelRef, ModelSettings, ProviderID, ProviderInfo,
    ProviderInfoActivation, ProviderSettings, SessionID,
};
use opencode_location::state::Registration;
use serde_json::{Value, json};

use crate::builtin::{Harness, Input, start};
use crate::provider_plugin::support::{
    Reply, Routes, plugin_context, plugin_context_with_runner, provider_settings_from, settings_json,
};

const AZURE: &str = "azure";

const RESPONSES: &str = "@opencode/ai/providers/azure/responses";

const OPENAI_COMPATIBLE: &str = "@opencode/ai/providers/openai-compatible";

const RESOURCE_ID: &str =
    "/subscriptions/sub/resourceGroups/rg/providers/Microsoft.CognitiveServices/accounts/test-resource";

const MANAGEMENT_BEARER: &str = "Bearer https://management.azure.com/.default-token";

const COGNITIVE_BEARER: &str = "Bearer https://cognitiveservices.azure.com/.default-token";

const FOUNDRY_BEARER: &str = "Bearer https://ai.azure.com/.default-token";

const HOUR_MS: i64 = 60 * 60 * 1000;

const CATALOG_IDS: [&str; 4] = ["deepseek-v4-flash", "gpt-5", "gpt-5-mini", "gpt-5-nano"];

struct FakeAzure {
    routes: Arc<Routes>,
    origin: String,
}

fn fake_azure() -> FakeAzure {
    let origin = "http://azure.test".to_owned();
    let routes = Routes::new();
    routes.route_json(
        "POST",
        &format!("{origin}/providers/Microsoft.ResourceGraph/resources?api-version=2022-10-01"),
        json!({ "data": [{ "id": RESOURCE_ID }] }),
    );
    FakeAzure { routes, origin }
}

impl FakeAzure {
    fn endpoints(&self) -> Endpoints {
        let origin = self.origin.clone();
        Endpoints {
            resource: Arc::new(move |name: &str| format!("{origin}/resource/{name}/openai")),
            management: self.origin.clone(),
        }
    }

    fn resource_url(&self, name: &str) -> String {
        format!("{}/resource/{name}/openai/deployments?api-version=2022-12-01", self.origin)
    }

    fn deployments_url(&self) -> String {
        format!("{}{}/deployments?api-version=2024-10-01", self.origin, RESOURCE_ID)
    }

    fn page_url(&self, path: &str) -> String {
        format!("{}{}", self.origin, path)
    }

    fn options(&self) -> Options {
        Options { endpoints: self.endpoints(), interval: None }
    }
}

fn listed(id: &str, model: &str, status: &str) -> Value {
    json!({ "id": id, "model": model, "status": status })
}

fn resource_list(deployments: Vec<Value>) -> Reply {
    Reply::json(json!({ "data": deployments }))
}

fn managed(name: &str, model: &str, state: &str) -> Value {
    json!({ "name": name, "properties": { "model": { "name": model }, "provisioningState": state } })
}

fn management_page(deployments: Vec<Value>, next: Option<String>) -> Reply {
    let mut page = json!({ "value": deployments });
    if let Some(next) = next {
        page["nextLink"] = Value::String(next);
    }
    Reply::json(page)
}

fn header<'a>(request: &'a PreparedHttp, name: &str) -> Option<&'a str> {
    request.headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
}

fn cli_token(args: &[String]) -> Option<Value> {
    let scope = args.iter().position(|arg| arg == "--scope").and_then(|index| args.get(index + 1))?;
    Some(json!({
        "accessToken": format!("{scope}-token"),
        "expires_on": (opencode_util::time::now_ms() + HOUR_MS) / 1000,
    }))
}

type Respond = dyn Fn(&[String]) -> Option<Value> + Send + Sync;

struct ScriptedRunner {
    respond: Box<Respond>,
    calls: Mutex<Vec<Vec<String>>>,
}

impl ScriptedRunner {
    fn new(respond: impl Fn(&[String]) -> Option<Value> + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self { respond: Box::new(respond), calls: Mutex::new(Vec::new()) })
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().expect("the az calls lock is never poisoned").clone()
    }
}

impl CommandRunner for ScriptedRunner {
    fn run(
        &self,
        program: String,
        args: Vec<String>,
        _env: Vec<(OsString, OsString)>,
    ) -> BoxFuture<io::Result<CommandOutput>> {
        self.calls.lock().expect("the az calls lock is never poisoned").push(args.clone());
        let value = (self.respond)(&args);
        Box::pin(async move {
            let Some(value) = value else {
                return Err(io::Error::new(io::ErrorKind::NotFound, format!("no recorded az output for {program}")));
            };
            Ok(CommandOutput {
                status: std::process::ExitStatus::default(),
                stdout: serde_json::to_vec(&value).expect("the az output serializes"),
                stderr: Vec::new(),
            })
        })
    }
}

fn cli_runner() -> Arc<ScriptedRunner> {
    ScriptedRunner::new(cli_token)
}

fn install_az(harness: &mut Harness) {
    std::fs::write(harness.root().join("az"), "").expect("write the az stub");
    let path = std::env::join_paths([harness.root()]).expect("the test PATH joins");
    harness.environment.insert("PATH".to_owned(), path.to_string_lossy().into_owned());
}

fn key_credential(key: &str, resource: &str) -> CredentialValue {
    CredentialValue::Key(CredentialKey {
        r#type: CredentialKeyType::Key,
        key: key.to_owned(),
        metadata: Some(JsonMap(vec![("resourceName".to_owned(), json!(resource))])),
    })
}

fn cli_credential(resource: &str) -> CredentialValue {
    CredentialValue::External(CredentialExternal {
        r#type: CredentialExternalType::External,
        method_id: IntegrationMethodID("azure-cli".to_owned()),
        metadata: Some(JsonMap(vec![("resourceName".to_owned(), json!(resource))])),
    })
}

async fn connect(harness: &Harness, value: CredentialValue) -> CredentialID {
    harness
        .services
        .credentials
        .create(CreateInput {
            id: None,
            integration_id: IntegrationID(AZURE.to_owned()),
            value,
            label: None,
            activate: None,
        })
        .expect("create the test credential")
        .id
}

async fn seed_provider(harness: &Harness, id: &str, package: &str, settings: Option<Value>) -> Registration {
    let id = ProviderID(id.to_owned());
    let package = package.to_owned();
    let settings = settings.map(provider_settings_from);
    harness
        .services
        .provider
        .transform(move |editor| {
            editor.update(&id, |provider| {
                provider.package = package.clone();
                if let Some(settings) = settings.clone() {
                    provider.settings = Some(settings);
                }
            });
            Ok(())
        })
        .await
}

fn model_settings(value: Value) -> ModelSettings {
    decode_json(&value.to_string()).expect("the model settings decode")
}

async fn seed_catalog(harness: &Harness) -> Vec<Registration> {
    let provider = seed_provider(harness, AZURE, RESPONSES, None).await;
    let provider_id = ProviderID(AZURE.to_owned());
    let mini = ModelInfoLimit { context: 400_000.0, input: None, output: 128_000.0 };
    let nano = ModelInfoLimit { context: 300_000.0, input: None, output: 64_000.0 };
    let deepseek = model_settings(json!({
        "baseURL": "https://${AZURE_RESOURCE_NAME}.services.ai.azure.com/models",
    }));
    let models = harness
        .services
        .provider
        .transform(move |editor| {
            editor.models_update(&provider_id, "gpt-5", |_| {});
            editor.models_update(&provider_id, "gpt-5-mini", |model| {
                model.name = "GPT-5 Mini".to_owned();
                model.limit = mini.clone();
            });
            editor.models_update(&provider_id, "gpt-5-nano", |model| {
                model.name = "GPT-5 Nano".to_owned();
                model.limit = nano.clone();
            });
            editor.models_update(&provider_id, "deepseek-v4-flash", |model| {
                model.name = "DeepSeek-V4-Flash".to_owned();
                model.package = Some(OPENAI_COMPATIBLE.to_owned());
                model.settings = Some(deepseek.clone());
            });
            Ok(())
        })
        .await;
    vec![provider, models]
}

async fn register(harness: &mut Harness, azure: &FakeAzure, runner: Arc<ScriptedRunner>, options: Options) {
    let context = plugin_context_with_runner(harness, azure.routes.clone(), runner, None);
    let mut builtin = Builtin::default();
    azure::register_with(&harness.services, &context, &mut builtin, options).await;
    harness.record(builtin);
}

async fn register_hooks(harness: &mut Harness, azure: &FakeAzure, runner: Arc<ScriptedRunner>) -> Arc<ProviderHooks> {
    let context = plugin_context_with_runner(harness, azure.routes.clone(), runner, None);
    let mut builtin = Builtin::default();
    azure::register_with(&harness.services, &context, &mut builtin, azure.options()).await;
    harness.record(builtin);
    context.hooks.clone()
}

fn header_map(entries: &[(&str, &str)]) -> IndexMap<String, String> {
    entries.iter().map(|(name, value)| ((*name).to_owned(), (*value).to_owned())).collect()
}

fn http_request(url: &str, headers: &[(&str, &str)]) -> HttpRequest {
    HttpRequest {
        session_id: SessionID("ses_azure".to_owned()),
        agent: AgentID("build".to_owned()),
        model: ModelRef { id: "gpt-5-mini".to_owned(), provider_id: ProviderID(AZURE.to_owned()), variant: None },
        kind: RequestKind::Primary,
        request: PreparedHttp::new(
            "POST",
            url,
            headers.iter().map(|(name, value)| ((*name).to_owned(), (*value).to_owned())).collect(),
            Bytes::new(),
        ),
    }
}

fn ws_handshake(url: &str, headers: &[(&str, &str)]) -> WsHandshake {
    WsHandshake {
        session_id: SessionID("ses_azure_ws".to_owned()),
        agent: AgentID("build".to_owned()),
        model: ModelRef { id: "gpt-5-mini".to_owned(), provider_id: ProviderID(AZURE.to_owned()), variant: None },
        kind: RequestKind::Primary,
        url: url.to_owned(),
        headers: header_map(headers),
    }
}

async fn provider(harness: &Harness, id: &str) -> ProviderInfo {
    harness.services.provider.get(&ProviderID(id.to_owned())).await.unwrap().unwrap()
}

async fn azure_models(harness: &Harness) -> Vec<ModelInfo> {
    let mut models: Vec<ModelInfo> =
        harness.services.model.all().await.unwrap().into_iter().filter(|model| model.provider_id.0 == AZURE).collect();
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models
}

async fn azure_model_ids(harness: &Harness) -> Vec<String> {
    azure_models(harness).await.into_iter().map(|model| model.id).collect()
}

async fn eventually_models(harness: &Harness, predicate: impl Fn(&[ModelInfo]) -> bool) -> Vec<ModelInfo> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let models = azure_models(harness).await;
            if predicate(&models) {
                return models;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the Azure models settle")
}

async fn eventually_ids(harness: &Harness, predicate: impl Fn(&[String]) -> bool) -> Vec<String> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let ids = azure_model_ids(harness).await;
            if predicate(&ids) {
                return ids;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the Azure model ids settle")
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(25)).await;
}

fn path_count(routes: &Routes, path: &str) -> usize {
    routes.requests().iter().filter(|request| request.url.contains(path)).count()
}

fn model_setting(model: &ModelInfo, key: &str) -> Option<String> {
    model.settings.as_ref()?.rest.0.iter().find(|(name, _)| name == key)?.1.as_str().map(str::to_owned)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hides_azure_cli_authentication_when_the_azure_cli_is_not_installed() {
    let mut harness = start(Input::new(Vec::new()).environment("PATH", "/nonexistent")).await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["azure"], context).await;

    let integration = harness.services.integration.get(&IntegrationID(AZURE.to_owned())).await.unwrap().unwrap();
    assert!(!integration.methods.iter().any(|method| matches!(method, IntegrationMethod::External(_))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn asks_for_the_resource_name_as_text_without_calling_azure() {
    let mut harness = start(Input::new(Vec::new())).await;
    install_az(&mut harness);
    let runner = cli_runner();
    let azure = fake_azure();
    register(&mut harness, &azure, runner.clone(), azure.options()).await;

    let integration = harness.services.integration.get(&IntegrationID(AZURE.to_owned())).await.unwrap().unwrap();
    let external = integration
        .methods
        .iter()
        .find_map(|method| match method {
            IntegrationMethod::External(external) => Some(external),
            _ => None,
        })
        .expect("the Azure CLI method is registered");
    let field = match external.form.as_ref().expect("the Azure CLI form").0.first().expect("the resource field") {
        FormField1::String(field) => field,
        other => panic!("expected a string field, found {other:?}"),
    };
    assert_eq!(field.key, "resourceName");
    assert_eq!(field.r#type, FormStringField1Type::String);
    assert!(field.options.is_none());

    settle().await;
    assert!(runner.calls().is_empty());
    assert!(azure.routes.requests().is_empty());

    let integration_id = IntegrationID(AZURE.to_owned());
    let method_id = IntegrationMethodID("azure-cli".to_owned());
    let invalid = harness
        .services
        .integration
        .connection()
        .external(ExternalInput {
            integration_id: integration_id.clone(),
            method_id: method_id.clone(),
            answer: Some(JsonMap(vec![("resourceName".to_owned(), FormValue::String("not a hostname".to_owned()))])),
            label: None,
        })
        .await;
    assert!(invalid.is_err());
    harness
        .services
        .integration
        .connection()
        .external(ExternalInput {
            integration_id: integration_id.clone(),
            method_id,
            answer: Some(JsonMap(vec![("resourceName".to_owned(), FormValue::String("typed-resource".to_owned()))])),
            label: None,
        })
        .await
        .expect("the typed resource login succeeds");
    let credentials = harness.services.credentials.list(&integration_id).unwrap();
    assert_eq!(
        credentials.into_iter().map(|credential| credential.value).collect::<Vec<_>>(),
        [cli_credential("typed-resource")]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn starts_without_calling_azure_then_lists_deployments() {
    let mut harness = start(Input::new(Vec::new())).await;
    install_az(&mut harness);
    let runner = cli_runner();
    let azure = fake_azure();
    azure.routes.route_json(
        "GET",
        &azure.deployments_url(),
        json!({ "value": [managed("gpt-5-mini", "gpt-5-mini", "Succeeded")] }),
    );
    azure.routes.hold("GET", &azure.deployments_url());
    let _seeded = seed_catalog(&harness).await;
    connect(&harness, cli_credential("test-resource")).await;
    register(&mut harness, &azure, runner.clone(), azure.options()).await;

    assert!(runner.calls().is_empty());
    assert!(azure.routes.requests().is_empty());
    let settings = settings_json(&provider(&harness, AZURE).await).expect("the Azure provider settings");
    assert_eq!(settings.get("resourceName").and_then(Value::as_str), Some("test-resource"));
    assert_eq!(azure_model_ids(&harness).await, CATALOG_IDS);

    azure.routes.release();
    assert_eq!(eventually_ids(&harness, |ids| ids.len() == 1).await, ["gpt-5-mini"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shows_the_deployments_listed_with_an_api_key_named_after_the_deployment() {
    let mut harness = start(Input::new(Vec::new())).await;
    let runner = cli_runner();
    let azure = fake_azure();
    azure.routes.route_with("GET", &azure.resource_url("test-resource"), |request| {
        if header(request, "api-key") != Some("secret") {
            return Reply::status(401);
        }
        resource_list(vec![
            listed("gpt-5-mini", "gpt-5-mini", "succeeded"),
            listed("prod-mini", "gpt-5-mini", "succeeded"),
            listed("GPT-5-Nano", "gpt-5-nano", "succeeded"),
            listed("gpt-5", "gpt-5-mini", "succeeded"),
            listed("deepseek-v4-flash", "DeepSeek-V4-Flash-2026", "succeeded"),
            listed("ft-legal", "gpt-4o-mini.ft-123", "succeeded"),
            listed("pending", "gpt-5", "creating"),
            json!({ "id": 42 }),
        ])
    });
    let _seeded = seed_catalog(&harness).await;
    connect(&harness, key_credential("secret", "test-resource")).await;
    register(&mut harness, &azure, runner, azure.options()).await;

    let deployed = eventually_models(&harness, |models| models.len() == 5).await;
    let rows: Vec<(String, String, String, f64)> = deployed
        .iter()
        .map(|model| (model.id.clone(), model.model_id.clone(), model.name.clone(), model.limit.context))
        .collect();
    assert_eq!(
        rows,
        [
            ("deepseek-v4-flash".to_owned(), "deepseek-v4-flash".to_owned(), "DeepSeek-V4-Flash".to_owned(), 200_000.0),
            ("gpt-5".to_owned(), "gpt-5".to_owned(), "GPT-5 Mini (gpt-5)".to_owned(), 400_000.0),
            ("gpt-5-mini".to_owned(), "gpt-5-mini".to_owned(), "GPT-5 Mini".to_owned(), 400_000.0),
            ("gpt-5-nano".to_owned(), "GPT-5-Nano".to_owned(), "GPT-5 Nano".to_owned(), 300_000.0),
            ("prod-mini".to_owned(), "prod-mini".to_owned(), "GPT-5 Mini (prod-mini)".to_owned(), 400_000.0),
        ]
    );
    assert_eq!(
        deployed.first().and_then(|model| model_setting(model, "baseURL")),
        Some("https://test-resource.services.ai.azure.com/models".to_owned())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lists_azure_cli_deployments_through_the_management_api_before_the_resources_own_list() {
    let mut harness = start(Input::new(Vec::new())).await;
    install_az(&mut harness);
    let runner = cli_runner();
    let azure = fake_azure();
    azure.routes.route_json(
        "GET",
        &azure.resource_url("test-resource"),
        json!({ "data": [listed("from-resource", "gpt-5-nano", "succeeded")] }),
    );
    azure.routes.route_with("GET", &azure.deployments_url(), |request| {
        if header(request, "authorization") != Some(MANAGEMENT_BEARER) {
            return Reply::status(401);
        }
        management_page(
            vec![managed("gpt-production", "gpt-5-mini", "Succeeded"), managed("gpt-5-nano", "gpt-5-nano", "Failed")],
            None,
        )
    });
    let _seeded = seed_catalog(&harness).await;
    connect(&harness, cli_credential("test-resource")).await;
    register(&mut harness, &azure, runner, azure.options()).await;

    let deployed = eventually_models(&harness, |models| models.len() == 1).await;
    assert_eq!(
        deployed.iter().map(|model| (model.id.clone(), model.name.clone())).collect::<Vec<_>>(),
        [("gpt-production".to_owned(), "GPT-5 Mini (gpt-production)".to_owned())]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn falls_back_to_the_resources_own_list_when_the_azure_cli_cannot_read_the_management_api() {
    let mut harness = start(Input::new(Vec::new())).await;
    install_az(&mut harness);
    let runner = cli_runner();
    let azure = fake_azure();
    azure.routes.route_with("GET", &azure.resource_url("test-resource"), |request| {
        if header(request, "authorization") != Some(COGNITIVE_BEARER) {
            return Reply::status(401);
        }
        resource_list(vec![listed("gpt-5-mini", "gpt-5-mini", "succeeded")])
    });
    azure.routes.route_with("GET", &azure.deployments_url(), |_| Reply::status(404));
    let _seeded = seed_catalog(&harness).await;
    connect(&harness, cli_credential("test-resource")).await;
    register(&mut harness, &azure, runner, azure.options()).await;

    assert_eq!(eventually_ids(&harness, |ids| ids.len() == 1).await, ["gpt-5-mini"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keeps_the_catalog_for_a_custom_endpoint_without_listing_deployments() {
    let mut harness = start(Input::new(Vec::new())).await;
    let azure = fake_azure();
    azure.routes.route_json(
        "GET",
        &azure.resource_url("test-resource"),
        json!({ "data": [listed("gpt-5-mini", "gpt-5-mini", "succeeded")] }),
    );
    let _seeded = seed_catalog(&harness).await;
    let _custom =
        seed_provider(&harness, AZURE, RESPONSES, Some(json!({ "baseURL": "https://gateway.example/azure" }))).await;
    connect(&harness, key_credential("secret", "test-resource")).await;
    register(&mut harness, &azure, cli_runner(), azure.options()).await;

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(azure.routes.requests().is_empty());
    assert_eq!(azure_model_ids(&harness).await, CATALOG_IDS);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publishes_the_management_list_only_after_its_last_page() {
    let mut harness = start(Input::new(Vec::new())).await;
    install_az(&mut harness);
    let runner = cli_runner();
    let azure = fake_azure();
    let next = azure.page_url("/page-2");
    let origin = azure.origin.clone();
    azure.routes.route_with("GET", &azure.deployments_url(), move |_| {
        management_page(vec![managed("mini", "gpt-5-mini", "Succeeded")], Some(format!("{origin}/page-2")))
    });
    azure.routes.route_json("GET", &next, json!({ "value": [managed("nano", "gpt-5-nano", "Succeeded")] }));
    azure.routes.hold("GET", &next);
    let _seeded = seed_catalog(&harness).await;
    connect(&harness, cli_credential("test-resource")).await;
    register(&mut harness, &azure, runner, azure.options()).await;

    azure.routes.arrived().await;
    assert_eq!(azure_model_ids(&harness).await, CATALOG_IDS);
    azure.routes.release();
    assert_eq!(eventually_ids(&harness, |ids| ids.len() == 2).await, ["mini", "nano"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keeps_the_last_complete_list_when_a_later_page_fails() {
    let mut harness = start(Input::new(Vec::new())).await;
    install_az(&mut harness);
    let runner = cli_runner();
    let azure = fake_azure();
    let paged = Arc::new(AtomicBool::new(false));
    let origin = azure.origin.clone();
    azure.routes.route_with("GET", &azure.deployments_url(), {
        let paged = paged.clone();
        move |_| {
            if paged.load(Ordering::SeqCst) {
                return management_page(
                    vec![managed("nano", "gpt-5-nano", "Succeeded")],
                    Some(format!("{origin}/page-2")),
                );
            }
            management_page(vec![managed("mini", "gpt-5-mini", "Succeeded")], None)
        }
    });
    let next = azure.page_url("/page-2");
    azure.routes.route_with("GET", &next, |_| Reply::status(503));
    let _seeded = seed_catalog(&harness).await;
    connect(&harness, cli_credential("test-resource")).await;
    register(&mut harness, &azure, runner, Options { interval: Some(Duration::from_millis(200)), ..azure.options() })
        .await;

    assert_eq!(eventually_ids(&harness, |ids| ids.len() == 1).await, ["mini"]);
    paged.store(true, Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if path_count(&azure.routes, "/page-2") >= 4 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the later page is retried");
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(azure_model_ids(&harness).await, ["mini"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_the_management_api_error_when_the_resources_own_list_fails_too() {
    let mut harness = start(Input::new(Vec::new())).await;
    install_az(&mut harness);
    let runner = cli_runner();
    let azure = fake_azure();
    let first = Arc::new(AtomicBool::new(true));
    azure.routes.route_with("GET", &azure.deployments_url(), {
        let first = first.clone();
        move |_| {
            if first.swap(false, Ordering::SeqCst) {
                return Reply::status(503);
            }
            management_page(vec![managed("mini", "gpt-5-mini", "Succeeded")], None)
        }
    });
    azure.routes.route_with("GET", &azure.resource_url("test-resource"), |_| Reply::status(404));
    let _seeded = seed_catalog(&harness).await;
    connect(&harness, cli_credential("test-resource")).await;
    register(&mut harness, &azure, runner, azure.options()).await;

    assert_eq!(eventually_ids(&harness, |ids| ids.len() == 1).await, ["mini"]);
    assert_eq!(path_count(&azure.routes, "/openai/deployments"), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn never_sends_the_management_token_to_a_page_link_outside_the_management_endpoint() {
    let mut harness = start(Input::new(Vec::new())).await;
    install_az(&mut harness);
    let runner = cli_runner();
    let azure = fake_azure();
    azure.routes.route_with("GET", &azure.deployments_url(), |_| {
        management_page(
            vec![managed("mini", "gpt-5-mini", "Succeeded")],
            Some("https://attacker.example/page-2".to_owned()),
        )
    });
    let fell_back = Arc::new(tokio::sync::Notify::new());
    azure.routes.route_with("GET", &azure.resource_url("test-resource"), {
        let fell_back = fell_back.clone();
        move |_| {
            fell_back.notify_one();
            Reply::status(404)
        }
    });
    let _seeded = seed_catalog(&harness).await;
    connect(&harness, cli_credential("test-resource")).await;
    register(&mut harness, &azure, runner, azure.options()).await;

    tokio::time::timeout(Duration::from_secs(5), fell_back.notified()).await.expect("the resource fallback arrives");
    settle().await;
    assert_eq!(azure_model_ids(&harness).await, CATALOG_IDS);
    assert!(azure.routes.requests().iter().all(|request| !request.url.contains("attacker.example")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keeps_a_model_that_is_configured_explicitly() {
    let config = json!({
        "providers": { "azure": { "models": { "gpt-5-nano": { "modelID": "nano-production" } } } }
    });
    let mut harness = start(Input::new(vec![config])).await;
    let azure = fake_azure();
    azure.routes.route_json(
        "GET",
        &azure.resource_url("test-resource"),
        json!({ "data": [listed("gpt-5-mini", "gpt-5-mini", "succeeded")] }),
    );
    let _seeded = seed_catalog(&harness).await;
    connect(&harness, key_credential("secret", "test-resource")).await;
    register(&mut harness, &azure, cli_runner(), azure.options()).await;
    harness.register(&["config_provider"]).await;

    let deployed = eventually_models(&harness, |models| models.len() == 2).await;
    let rows: Vec<(String, String, f64)> =
        deployed.iter().map(|model| (model.id.clone(), model.model_id.clone(), model.limit.context)).collect();
    assert_eq!(
        rows,
        [
            ("gpt-5-mini".to_owned(), "gpt-5-mini".to_owned(), 400_000.0),
            ("gpt-5-nano".to_owned(), "nano-production".to_owned(), 300_000.0),
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loads_again_when_the_api_key_changes_in_place() {
    let mut harness = start(Input::new(Vec::new())).await;
    let azure = fake_azure();
    azure.routes.route_with("GET", &azure.resource_url("test-resource"), |request| {
        let name = if header(request, "api-key") == Some("secret") { "production-a" } else { "production-b" };
        resource_list(vec![listed(name, "gpt-5-mini", "succeeded")])
    });
    let _seeded = seed_catalog(&harness).await;
    let credential = connect(&harness, key_credential("secret", "test-resource")).await;
    register(&mut harness, &azure, cli_runner(), azure.options()).await;
    eventually_ids(&harness, |ids| ids.contains(&"production-a".to_owned())).await;

    harness
        .services
        .credentials
        .update(&credential, Update { label: None, value: Some(key_credential("rotated-key", "test-resource")) })
        .expect("update the stored credential");
    harness
        .services
        .bus
        .publish(
            &Definition::ephemeral("credential.switched"),
            json!({ "integrationID": AZURE, "credentialID": credential.0 }),
            PublishOptions { global: true, ..Default::default() },
        )
        .expect("publish the credential switch");

    eventually_ids(&harness, |ids| ids.contains(&"production-b".to_owned())).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shows_the_new_account_without_waiting_for_the_previous_accounts_discovery() {
    let mut harness = start(Input::new(Vec::new())).await;
    let azure = fake_azure();
    azure.routes.route_json(
        "GET",
        &azure.resource_url("test-resource"),
        json!({ "data": [listed("stale", "gpt-5-nano", "succeeded")] }),
    );
    azure.routes.hold("GET", &azure.resource_url("test-resource"));
    azure.routes.route_json(
        "GET",
        &azure.resource_url("other-resource"),
        json!({ "data": [listed("production-b", "gpt-5-mini", "succeeded")] }),
    );
    let _seeded = seed_catalog(&harness).await;
    connect(&harness, key_credential("secret", "test-resource")).await;
    register(&mut harness, &azure, cli_runner(), azure.options()).await;

    azure.routes.arrived().await;
    connect(&harness, key_credential("other-key", "other-resource")).await;

    let deployed = eventually_models(&harness, |models| {
        models.len() == 1 && models.first().map(|model| model.id.as_str()) == Some("production-b")
    })
    .await;
    assert_eq!(
        deployed.first().and_then(|model| model_setting(model, "resourceName")),
        Some("other-resource".to_owned())
    );

    azure.routes.release();
    settle().await;
    assert_eq!(azure_model_ids(&harness).await, ["production-b"]);
}

struct ResourceCase {
    name: &'static str,
    env: &'static [(&'static str, &'static str)],
    settings: Option<Value>,
    credential: Option<CredentialValue>,
    expected: &'static str,
}

async fn run_resource_case(case: ResourceCase) {
    let mut input = Input::new(Vec::new());
    for (name, value) in case.env {
        input = input.environment(name, value);
    }
    let mut harness = start(input).await;
    let azure = fake_azure();
    azure.routes.route_with("GET", &azure.resource_url("test-resource"), |_| Reply::status(404));
    let _seeded = seed_provider(&harness, AZURE, RESPONSES, case.settings).await;
    let openai = ProviderID("openai".to_owned());
    let _openai = harness
        .services
        .provider
        .transform(move |editor| {
            editor.update(&openai, |_| {});
            Ok(())
        })
        .await;
    if let Some(credential) = case.credential {
        connect(&harness, credential).await;
    }
    register(&mut harness, &azure, cli_runner(), azure.options()).await;

    let settings = settings_json(&provider(&harness, AZURE).await).expect("the Azure provider settings");
    assert_eq!(settings.get("resourceName").and_then(Value::as_str), Some(case.expected), "{}", case.name);
    let settings = settings_json(&provider(&harness, "openai").await);
    assert_eq!(
        settings.as_ref().and_then(|settings| settings.get("resourceName")).and_then(Value::as_str),
        None,
        "{}",
        case.name
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reads_the_azure_resource_name_from_the_environment_and_configuration() {
    let cases = [
        ResourceCase {
            name: "reads AZURE_RESOURCE_NAME",
            env: &[("AZURE_RESOURCE_NAME", "from-env")],
            settings: None,
            credential: None,
            expected: "from-env",
        },
        ResourceCase {
            name: "reads the legacy AZURE_COGNITIVE_SERVICES_RESOURCE_NAME",
            env: &[("AZURE_COGNITIVE_SERVICES_RESOURCE_NAME", "legacy-resource")],
            settings: None,
            credential: None,
            expected: "legacy-resource",
        },
        ResourceCase {
            name: "prefers configuration over the environment",
            env: &[("AZURE_RESOURCE_NAME", "from-env")],
            settings: Some(json!({ "resourceName": "from-config" })),
            credential: None,
            expected: "from-config",
        },
        ResourceCase {
            name: "ignores a whitespace configured name",
            env: &[("AZURE_RESOURCE_NAME", "from-env")],
            settings: Some(json!({ "resourceName": "   " })),
            credential: None,
            expected: "from-env",
        },
        ResourceCase {
            name: "prefers the connections resource over configuration",
            env: &[("AZURE_RESOURCE_NAME", "from-env")],
            settings: Some(json!({ "resourceName": "from-config" })),
            credential: Some(key_credential("secret", "test-resource")),
            expected: "test-resource",
        },
        ResourceCase {
            name: "reads the resource of an API key imported from V1",
            env: &[("AZURE_RESOURCE_NAME", "from-env")],
            settings: None,
            credential: Some(key_credential("secret", "imported-resource")),
            expected: "imported-resource",
        },
    ];
    for case in cases {
        run_resource_case(case).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn leaves_models_unchanged_and_uncloned_without_a_resource_name() {
    let mut harness = start(Input::new(Vec::new())).await;
    let azure = fake_azure();
    let _seeded = seed_catalog(&harness).await;
    let provider_id = ProviderID(AZURE.to_owned());
    let _enabled = harness
        .services
        .provider
        .transform(move |editor| {
            editor.update(&provider_id, |provider| provider.activation = ProviderInfoActivation::Enabled);
            Ok(())
        })
        .await;
    register(&mut harness, &azure, cli_runner(), azure.options()).await;

    let before = azure_models(&harness).await;
    let deepseek = before.iter().find(|model| model.id == "deepseek-v4-flash").expect("the DeepSeek model");
    assert_eq!(
        model_setting(deepseek, "baseURL").as_deref(),
        Some("https://${AZURE_RESOURCE_NAME}.services.ai.azure.com/models")
    );
    harness.services.model.reload().await.unwrap();
    let after = azure_models(&harness).await;
    assert_eq!(after, before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expands_provider_and_model_resource_urls() {
    let mut input = Input::new(Vec::new());
    input = input.environment("AZURE_RESOURCE_NAME", "from-env");
    input = input.environment("AZURE_COGNITIVE_SERVICES_RESOURCE_NAME", "legacy-env");
    let mut harness = start(input).await;
    let azure = fake_azure();
    let provider_id = ProviderID(AZURE.to_owned());
    let seeded = provider_id.clone();
    let anthropic = model_settings(json!({
        "resourceName": "model-resource",
        "baseURL": "https://${AZURE_RESOURCE_NAME}.services.ai.azure.com/anthropic/v1",
    }));
    let settings: ProviderSettings = provider_settings_from(json!({
        "baseURL": "https://${AZURE_COGNITIVE_SERVICES_RESOURCE_NAME}.cognitiveservices.azure.com/openai",
    }));
    let _seeded = harness
        .services
        .provider
        .transform(move |editor| {
            editor.update(&seeded, |provider| {
                provider.package = OPENAI_COMPATIBLE.to_owned();
                provider.activation = ProviderInfoActivation::Enabled;
                provider.settings = Some(settings.clone());
            });
            editor.models_update(&seeded, "gpt-5-mini", |_| {});
            editor.models_update(&seeded, "anthropic", |model| {
                model.package = Some("@opencode/ai/providers/anthropic".to_owned());
                model.settings = Some(anthropic.clone());
            });
            Ok(())
        })
        .await;
    register(&mut harness, &azure, cli_runner(), azure.options()).await;

    let settings = settings_json(&provider(&harness, AZURE).await).expect("the Azure provider settings");
    assert_eq!(settings.get("resourceName").and_then(Value::as_str), Some("from-env"));
    assert_eq!(
        settings.get("baseURL").and_then(Value::as_str),
        Some("https://from-env.cognitiveservices.azure.com/openai")
    );
    let model = harness.services.model.get(&provider_id, "anthropic").await.unwrap().unwrap();
    assert_eq!(model_setting(&model, "resourceName").as_deref(), Some("model-resource"));
    assert_eq!(
        model_setting(&model, "baseURL").as_deref(),
        Some("https://model-resource.services.ai.azure.com/anthropic/v1")
    );
    let mini = harness.services.model.get(&provider_id, "gpt-5-mini").await.unwrap().unwrap();
    let before = model_setting(&mini, "baseURL");
    harness.services.model.reload().await.unwrap();
    let mini = harness.services.model.get(&provider_id, "gpt-5-mini").await.unwrap().unwrap();
    assert_eq!(model_setting(&mini, "baseURL"), before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prefers_websockets_on_the_provider_for_the_azure_responses_api_only() {
    let mut harness = start(Input::new(Vec::new())).await;
    let azure = fake_azure();
    let provider_id = ProviderID(AZURE.to_owned());
    let seeded = provider_id.clone();
    let preview = model_settings(json!({ "apiVersion": "2025-04-01-preview" }));
    let deployment = model_settings(json!({ "useDeploymentBasedUrls": true }));
    let gateway = model_settings(json!({ "baseURL": "https://gateway.example/azure" }));
    let _seeded = harness
        .services
        .provider
        .transform(move |editor| {
            editor.update(&seeded, |provider| {
                provider.package = RESPONSES.to_owned();
                provider.activation = ProviderInfoActivation::Enabled;
            });
            editor.models_update(&seeded, "responses", |_| {});
            editor.models_update(&seeded, "chat", |model| {
                model.package = Some("@opencode/ai/providers/azure/chat".to_owned());
            });
            editor.models_update(&seeded, "preview", |model| model.settings = Some(preview.clone()));
            editor.models_update(&seeded, "deployment-url", |model| model.settings = Some(deployment.clone()));
            editor.models_update(&seeded, "gateway", |model| model.settings = Some(gateway.clone()));
            editor.models_update(&seeded, "non-azure", |model| {
                model.package = Some("@opencode/ai/providers/anthropic".to_owned());
            });
            Ok(())
        })
        .await;
    register(&mut harness, &azure, cli_runner(), azure.options()).await;

    let settings = settings_json(&provider(&harness, AZURE).await).expect("the Azure provider settings");
    assert_eq!(settings.get("transport").and_then(Value::as_str), Some("websocket"));
    let models = harness.services.model.all().await.unwrap();
    for id in ["responses", "chat", "preview", "deployment-url", "gateway", "non-azure"] {
        let model = models.iter().find(|model| model.provider_id == provider_id && model.id == id);
        assert!(model.and_then(|model| model_setting(model, "transport")).is_none(), "{id}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authorizes_azure_foundry_and_websocket_requests_with_a_token_for_their_audience() {
    let mut harness = start(Input::new(Vec::new())).await;
    install_az(&mut harness);
    let azure = fake_azure();
    let runner = cli_runner();
    connect(&harness, cli_credential("test-resource")).await;
    let hooks = register_hooks(&mut harness, &azure, runner).await;

    let request = hooks
        .trigger_http_request(http_request(
            "https://test-resource.openai.azure.com/openai/v1/responses",
            &[("api-key", "stored-token"), ("x-keep", "yes")],
        ))
        .await;
    assert_eq!(header(&request.request, "authorization"), Some(COGNITIVE_BEARER));
    assert_eq!(header(&request.request, "api-key"), None);
    assert_eq!(header(&request.request, "x-keep"), Some("yes"));

    let foundry = hooks
        .trigger_http_request(http_request(
            "https://test-resource.services.ai.azure.com/anthropic/v1/messages",
            &[("x-api-key", "stored-token")],
        ))
        .await;
    assert_eq!(header(&foundry.request, "authorization"), Some(FOUNDRY_BEARER));
    assert_eq!(header(&foundry.request, "x-api-key"), None);

    let handshake = hooks
        .trigger_ws_handshake(ws_handshake(
            "wss://test-resource.openai.azure.com/openai/v1/responses",
            &[("api-key", "stored-token"), ("x-keep", "yes")],
        ))
        .await;
    assert_eq!(handshake.headers.get("authorization").map(String::as_str), Some(COGNITIVE_BEARER));
    assert_eq!(handshake.headers.get("x-keep").map(String::as_str), Some("yes"));
    assert!(!handshake.headers.contains_key("api-key"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uses_the_cognitive_scope_for_models_endpoints_and_leaves_api_key_requests_untouched() {
    let mut harness = start(Input::new(Vec::new())).await;
    install_az(&mut harness);
    let azure = fake_azure();
    let runner = cli_runner();
    connect(&harness, cli_credential("test-resource")).await;
    let hooks = register_hooks(&mut harness, &azure, runner).await;

    let models = hooks
        .trigger_http_request(http_request(
            "https://test-resource.services.ai.azure.com/models",
            &[("api-key", "stored-token")],
        ))
        .await;
    assert_eq!(header(&models.request, "authorization"), Some(COGNITIVE_BEARER));
    assert_eq!(header(&models.request, "api-key"), None);

    let mut keyed = start(Input::new(Vec::new())).await;
    let azure = fake_azure();
    connect(&keyed, key_credential("secret", "test-resource")).await;
    let hooks = register_hooks(&mut keyed, &azure, cli_runner()).await;

    let request = hooks
        .trigger_http_request(http_request(
            "https://test-resource.openai.azure.com/openai/v1/responses",
            &[("api-key", "stored-token")],
        ))
        .await;
    assert_eq!(header(&request.request, "api-key"), Some("stored-token"));
    assert_eq!(header(&request.request, "authorization"), None);
}
