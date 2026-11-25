use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use opencode_bus::{Bus, BusOptions};
use opencode_catalog::builtin::{self, Builtin, Services};
use opencode_catalog::credential::{CreateInput, CredentialService};
use opencode_catalog::integration::{ConfigEnv, IntegrationService};
use opencode_catalog::model::ModelService;
use opencode_catalog::models_dev::service::{ModelsDevService, Options};
use opencode_catalog::provider::ProviderService;
use opencode_catalog::provider_plugin::PluginContext;
use opencode_config::options::{ConfigLocation, ConfigOptions, SystemOption};
use opencode_config::paths::GlobalPaths;
use opencode_config::seams::{BoxFuture, KvStore};
use opencode_config::service::{ConfigService, ServiceDeps};
use opencode_config::wellknown::WellKnown;
use opencode_contract::generated::{AbsolutePath, CredentialValue, LocationRef, ProviderID};
use opencode_storage::{Database, DatabaseLocation};
use serde_json::{Map, Value};

use crate::TempDir;

#[derive(Default)]
struct TestKv {
    values: Mutex<Map<String, Value>>,
}

impl KvStore for TestKv {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Option<Value>> {
        Box::pin(async move { self.values.lock().expect("the test kv lock is never poisoned").get(key).cloned() })
    }

    fn set<'a>(&'a self, key: &'a str, value: Value) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.values.lock().expect("the test kv lock is never poisoned").insert(key.to_owned(), value);
        })
    }

    fn remove<'a>(&'a self, key: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.values.lock().expect("the test kv lock is never poisoned").remove(key);
        })
    }
}

pub struct Harness {
    _temp: TempDir,
    pub services: Services,
    pub registrations: Builtin,
    pub environment: HashMap<String, String>,
}

pub struct Input {
    pub entries: Vec<Value>,
    pub models: Option<Value>,
    pub url: Option<String>,
    pub environment: HashMap<String, String>,
}

impl Input {
    pub fn new(entries: Vec<Value>) -> Self {
        Self { entries, models: None, url: None, environment: HashMap::new() }
    }

    pub fn models(mut self, models: Value) -> Self {
        self.models = Some(models);
        self
    }

    pub fn url(mut self, url: &str) -> Self {
        self.url = Some(url.to_owned());
        self
    }

    pub fn environment(mut self, name: &str, value: &str) -> Self {
        self.environment.insert(name.to_owned(), value.to_owned());
        self
    }
}

impl Harness {
    pub async fn start(input: Input) -> Self {
        let temp = TempDir::new();
        let root = opencode_util::path::realpath(temp.path()).expect("the harness root resolves");
        let home = root.join("home");
        let config = root.join("global");
        let project = root.join("project");
        for directory in [&home, &config, &project] {
            std::fs::create_dir_all(directory).expect("create the harness directory");
        }
        if let Some(first) = input.entries.first() {
            std::fs::write(config.join("opencode.json"), serde_json::to_string(first).expect("json"))
                .expect("write the global config");
        }
        if let Some(second) = input.entries.get(1) {
            std::fs::write(project.join("opencode.json"), serde_json::to_string(second).expect("json"))
                .expect("write the project config");
        }
        let content = input.entries.get(2).map(|value| serde_json::to_string(value).expect("json"));
        let models_file = input.models.as_ref().map(|models| {
            let path = root.join("models.json");
            std::fs::write(&path, serde_json::to_string(models).expect("json")).expect("write the models catalog");
            path
        });

        let (database, _) = Database::open(DatabaseLocation::Memory, &root).expect("open the database");
        let database = Arc::new(database);
        let bus = Bus::new(database.clone(), BusOptions::default());
        let values = input.environment.clone();
        let environment: ConfigEnv = Arc::new(move |name| values.get(name).map(|value| OsString::from(value.clone())));
        let command_env: Arc<Vec<(OsString, OsString)>> = Arc::new(
            input.environment.iter().map(|(name, value)| (OsString::from(name), OsString::from(value))).collect(),
        );

        let credentials = CredentialService::new(database.clone(), bus.clone(), root.join("locks"));
        let wellknown = Arc::new(WellKnown::new(Arc::new(TestKv::default())));
        let paths = GlobalPaths::from_vars(|name| match name {
            "HOME" => Some(home.clone().into_os_string()),
            "OPENCODE_CONFIG_DIR" => Some(config.clone().into_os_string()),
            _ => None,
        })
        .expect("the harness paths resolve");
        let config_service = ConfigService::start(
            ConfigOptions {
                project: Some(true),
                global: Some(true),
                system: Some(SystemOption::Disabled),
                claude: Some(false),
                file: None,
                content,
            },
            ConfigLocation { directory: project.clone(), project_id: "test".to_owned() },
            paths,
            ServiceDeps {
                credentials: credentials.clone(),
                wellknown: wellknown.clone(),
                channel: "local".to_owned(),
                flag_overrides: None,
            },
        )
        .await;
        let location =
            LocationRef { directory: AbsolutePath(project.to_string_lossy().into_owned()), workspace_id: None };
        let integration = IntegrationService::new(
            credentials.clone(),
            bus.clone(),
            location.clone(),
            environment.clone(),
            command_env,
        );
        let provider = ProviderService::new(integration.clone(), bus.clone(), location.clone());
        let model = ModelService::new(provider.clone(), bus.clone(), location, environment);
        let models_dev = ModelsDevService::new(
            database.clone(),
            bus.clone(),
            Options {
                url: input.url.clone(),
                file: models_file,
                fetch: Some(input.url.is_some()),
                snapshot: Some(false),
            },
            "opencode/test".to_owned(),
        );
        let services =
            Services { bus, config: config_service, credentials, models_dev, wellknown, integration, provider, model };
        Self { _temp: temp, services, registrations: builtin::Builtin::default(), environment: input.environment }
    }

    pub async fn credential(&self, integration_id: &str, value: CredentialValue) {
        self.services
            .credentials
            .create(CreateInput {
                id: None,
                integration_id: opencode_contract::generated::IntegrationID(integration_id.to_owned()),
                value,
                label: None,
                activate: None,
            })
            .expect("create the test credential");
    }

    pub fn record(&mut self, builtin: Builtin) {
        self.registrations.record(builtin);
    }

    pub async fn register(&mut self, plugins: &[&str]) {
        let mut builtin = Builtin::default();
        for name in ["wellknown", "models_dev", "config_provider", "config_policy"] {
            if !plugins.contains(&name) {
                continue;
            }
            match name {
                "wellknown" => builtin::wellknown::register(&self.services, &mut builtin).await,
                "models_dev" => builtin::models_dev::register(&self.services, &mut builtin).await,
                "config_provider" => builtin::config_provider::register(&self.services, &mut builtin).await,
                "config_policy" => builtin::config_policy::register(&self.services, &mut builtin).await,
                _ => unreachable!(),
            }
        }
        self.record(builtin);
    }

    pub async fn register_provider_plugins(&mut self, names: &[&str], context: PluginContext) {
        let mut builtin = Builtin::default();
        for name in names {
            match *name {
                "amazon_bedrock" => {
                    opencode_catalog::provider_plugin::amazon_bedrock::register(&self.services, &context, &mut builtin)
                        .await
                }
                "azure" => {
                    opencode_catalog::provider_plugin::azure::register(&self.services, &context, &mut builtin).await
                }
                "cloudflare_ai_gateway" => {
                    opencode_catalog::provider_plugin::cloudflare_ai_gateway::register(
                        &self.services,
                        &context,
                        &mut builtin,
                    )
                    .await
                }
                "cloudflare_workers_ai" => {
                    opencode_catalog::provider_plugin::cloudflare_workers_ai::register(
                        &self.services,
                        &context,
                        &mut builtin,
                    )
                    .await
                }
                "cerebras" => {
                    opencode_catalog::provider_plugin::cerebras::register(&self.services, &context, &mut builtin).await
                }
                "prompt_cache_key" => {
                    opencode_catalog::provider_plugin::prompt_cache_key::register(
                        &self.services,
                        &context,
                        &mut builtin,
                    )
                    .await
                }
                "neon" => {
                    opencode_catalog::provider_plugin::neon::register(&self.services, &context, &mut builtin).await
                }
                "kilo" => {
                    opencode_catalog::provider_plugin::kilo::register(&self.services, &context, &mut builtin).await
                }
                "llmgateway" => {
                    opencode_catalog::provider_plugin::llmgateway::register(&self.services, &context, &mut builtin)
                        .await
                }
                "lmstudio" => {
                    opencode_catalog::provider_plugin::lmstudio::register(&self.services, &context, &mut builtin).await
                }
                "modal" => {
                    opencode_catalog::provider_plugin::modal::register(&self.services, &context, &mut builtin).await
                }
                "nvidia" => {
                    opencode_catalog::provider_plugin::nvidia::register(&self.services, &context, &mut builtin).await
                }
                "ollama" => {
                    opencode_catalog::provider_plugin::ollama::register(&self.services, &context, &mut builtin).await
                }
                "vllm" => {
                    opencode_catalog::provider_plugin::vllm::register(&self.services, &context, &mut builtin).await
                }
                "openrouter" => {
                    opencode_catalog::provider_plugin::openrouter::register(&self.services, &context, &mut builtin)
                        .await
                }
                "zenmux" => {
                    opencode_catalog::provider_plugin::zenmux::register(&self.services, &context, &mut builtin).await
                }
                "databricks" => {
                    opencode_catalog::provider_plugin::databricks::register(&self.services, &context, &mut builtin)
                        .await
                }
                "digitalocean" => {
                    opencode_catalog::provider_plugin::digitalocean::register(&self.services, &context, &mut builtin)
                        .await
                }
                "github_copilot" => {
                    opencode_catalog::provider_plugin::github_copilot::register(&self.services, &context, &mut builtin)
                        .await
                }
                "gitlab" => {
                    opencode_catalog::provider_plugin::gitlab::register(&self.services, &context, &mut builtin).await
                }
                "google_vertex" => {
                    opencode_catalog::provider_plugin::google_vertex::register(&self.services, &context, &mut builtin)
                        .await
                }
                "openai_context_caps" => {
                    opencode_catalog::provider_plugin::openai_context_caps::register(
                        &self.services,
                        &context,
                        &mut builtin,
                    )
                    .await
                }
                _ => panic!("unknown provider plugin: {name}"),
            }
        }
        self.record(builtin);
    }

    pub fn root(&self) -> PathBuf {
        opencode_util::path::realpath(self._temp.path()).expect("the harness root resolves")
    }
}

pub async fn start(input: Input) -> Harness {
    Harness::start(input).await
}

pub fn acme_catalog(name: &str) -> Value {
    serde_json::json!({
        "acme": {
            "id": "acme",
            "name": name,
            "env": ["ACME_API_KEY"],
            "models": {
                "acme-1": {
                    "id": "acme-1",
                    "type": "chat",
                    "name": "Acme One",
                    "release_date": "2026-01-01",
                    "modalities": { "input": ["text"], "output": ["text"] },
                    "capabilities": { "tools": { "supported": true }, "reasoning": { "supported": false } },
                    "limit": { "context": 128000, "output": 8192 },
                    "api": {
                        "generate-content": {
                            "base_url": "https://api.acme.test/v1",
                            "sdk": { "opencodeai": "@opencode/ai/providers/openai-compatible" }
                        }
                    }
                }
            }
        }
    })
}

fn providers_config() -> Value {
    serde_json::json!({
        "providers": {
            "custom": {
                "package": "aisdk:@ai-sdk/openai-compatible",
                "models": { "chat": {} }
            }
        }
    })
}

pub async fn enable(harness: &Harness) -> opencode_location::state::Registration {
    harness
        .services
        .provider
        .transform(|editor| {
            for record in editor.list() {
                editor.update(&record.provider.id, |provider| {
                    provider.activation = opencode_contract::generated::ProviderInfoActivation::Enabled;
                });
            }
            Ok(())
        })
        .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn models_dev_reloads_the_registries_after_a_refresh() {
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicBool, Ordering};

    let upgraded = Arc::new(AtomicBool::new(false));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind the catalog server");
    let origin = format!("http://{}", listener.local_addr().expect("the catalog address"));
    let stop = Arc::new(AtomicBool::new(false));
    let server = {
        let upgraded = upgraded.clone();
        let stop = stop.clone();
        listener.set_nonblocking(true).expect("the catalog listener is non-blocking");
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                };
                let mut request = [0u8; 4096];
                let _ = stream.read(&mut request);
                let name = if upgraded.load(Ordering::SeqCst) { "Acme Refreshed" } else { "Acme" };
                let body = serde_json::to_string(&acme_catalog(name)).expect("json");
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        })
    };

    let mut harness = start(Input::new(Vec::new()).url(&origin)).await;
    harness.register(&["models_dev"]).await;
    let _enabled = enable(&harness).await;
    let provider = harness.services.provider.get(&ProviderID("acme".to_owned())).await.unwrap().unwrap();
    assert_eq!(provider.name, "Acme");

    upgraded.store(true, Ordering::SeqCst);
    harness.services.models_dev.refresh(true).await;

    for _ in 0..200 {
        let snapshot = harness.services.provider.get(&ProviderID("acme".to_owned())).await.unwrap().unwrap();
        if snapshot.name == "Acme Refreshed" {
            stop.store(true, Ordering::SeqCst);
            server.join().expect("the catalog server");
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the refreshed provider name did not arrive");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_provider_reloads_when_the_config_changes() {
    let mut harness = start(Input::new(vec![providers_config()])).await;
    harness.register(&["config_provider"]).await;
    let models = harness.services.model.all().await.unwrap();
    assert_eq!(models.iter().map(|model| model.id.as_str()).collect::<Vec<_>>(), ["chat"]);

    let mut updated = providers_config();
    updated["providers"]["custom"]["models"]["second"] = serde_json::json!({});
    std::fs::write(
        harness.root().join("project").join("opencode.json"),
        serde_json::to_string(&updated).expect("json"),
    )
    .unwrap();
    harness.services.config.reload().await;

    let model = harness.services.model.clone();
    for _ in 0..200 {
        let ids: Vec<String> = model.all().await.unwrap().into_iter().map(|model| model.id).collect();
        if ids.contains(&"second".to_owned()) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the configured model did not arrive");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wellknown_adds_a_login_command_named_after_the_origin_host() {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind the manifest server");
    let origin = format!("http://{}", listener.local_addr().expect("the manifest address"));
    let manifest = serde_json::json!({
        "auth": { "command": ["acme", "login"], "env": "ACME_TOKEN" }
    });
    let server = {
        let manifest = serde_json::to_string(&manifest).unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept the manifest request");
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                manifest.len(),
                manifest
            );
            stream.write_all(response.as_bytes()).expect("write the manifest");
        })
    };

    let mut harness = start(Input::new(Vec::new())).await;
    harness.services.wellknown.add(&origin).await.expect("register the well known origin");
    harness.register(&["wellknown"]).await;
    server.join().expect("the manifest server");

    let integrations = harness.services.integration.list().await.unwrap();
    let integration = integrations.iter().find(|integration| integration.id.0 == origin).expect("the integration");
    assert_eq!(integration.name, "127.0.0.1");
    let command = integration.methods.iter().find_map(|method| match method {
        opencode_contract::generated::IntegrationMethod::Command(command) => Some(command),
        _ => None,
    });
    let command = command.expect("the login command");
    assert_eq!(command.id.0, "login");
    assert_eq!(command.label, "Log in");
    assert_eq!(command.command, ["acme", "login"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_policy_denies_an_integration_whose_account_is_blocked() {
    use opencode_contract::codec::JsonMap;
    use opencode_contract::generated::{CredentialKey, CredentialKeyType, CredentialValue};

    let mut harness = start(Input::new(vec![serde_json::json!({
        "policies": [{ "action": "integration.use", "resource": "custom#org-1", "effect": "deny" }],
        "providers": {
            "custom": { "package": "aisdk:@ai-sdk/openai-compatible", "models": { "chat": {} } },
            "kept": { "package": "aisdk:@ai-sdk/openai-compatible", "models": { "chat": {} } }
        }
    })]))
    .await;
    harness
        .credential(
            "custom",
            CredentialValue::Key(CredentialKey {
                r#type: CredentialKeyType::Key,
                key: "secret".to_owned(),
                metadata: Some(JsonMap(vec![("orgID".to_owned(), serde_json::json!("org-1"))])),
            }),
        )
        .await;
    harness.register(&["config_provider", "config_policy"]).await;

    let ids: Vec<String> =
        harness.services.provider.all().await.unwrap().into_iter().map(|provider| provider.id.0).collect();
    assert_eq!(ids, ["kept"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn builtin_register_composes_every_transform_in_order() {
    let mut harness = start(Input::new(vec![providers_config()]).models(acme_catalog("Acme"))).await;
    let builtin = builtin::register(
        Services {
            bus: harness.services.bus.clone(),
            config: harness.services.config.clone(),
            credentials: harness.services.credentials.clone(),
            models_dev: harness.services.models_dev.clone(),
            wellknown: harness.services.wellknown.clone(),
            integration: harness.services.integration.clone(),
            provider: harness.services.provider.clone(),
            model: harness.services.model.clone(),
        },
        builtin::Deps::default(),
    )
    .await;
    harness.record(builtin);

    assert_eq!(harness.registrations.registrations.len(), 7);
    assert_eq!(harness.registrations.observers.len(), 2);
    assert_eq!(harness.registrations.tasks.len(), 3);
    let ids: Vec<String> =
        harness.services.provider.all().await.unwrap().into_iter().map(|provider| provider.id.0).collect();
    assert_eq!(ids, ["acme", "custom"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn builtin_register_inserts_provider_plugins_at_the_provider_slot() {
    let mut harness = start(Input::new(vec![providers_config()]).models(acme_catalog("Acme"))).await;
    let context = crate::provider_plugin::support::plugin_context(&harness);
    let builtin = builtin::register(
        Services {
            bus: harness.services.bus.clone(),
            config: harness.services.config.clone(),
            credentials: harness.services.credentials.clone(),
            models_dev: harness.services.models_dev.clone(),
            wellknown: harness.services.wellknown.clone(),
            integration: harness.services.integration.clone(),
            provider: harness.services.provider.clone(),
            model: harness.services.model.clone(),
        },
        builtin::Deps { provider_plugins: Some(context), external: None },
    )
    .await;
    harness.record(builtin);
    assert_eq!(harness.registrations.registrations.len(), 56);
}

pub async fn seed(
    harness: &Harness,
    provider_id: &str,
    package: &str,
    models: Vec<Value>,
) -> opencode_location::state::Registration {
    use opencode_contract::codec::decode_json;
    use opencode_contract::generated::ModelInfo;

    let id = ProviderID(provider_id.to_owned());
    let package = package.to_owned();
    let models: Vec<ModelInfo> = models
        .into_iter()
        .map(|model| decode_json(&serde_json::to_string(&model).expect("json")).expect("a catalog model"))
        .collect();
    harness
        .services
        .provider
        .transform(move |editor| {
            editor.update(&id, |provider| provider.package = package.to_owned());
            editor.models_set(&id, &models);
            Ok(())
        })
        .await
}

pub fn catalog_model(id: &str, provider_id: &str, package: &str) -> Value {
    serde_json::json!({
        "id": id,
        "modelID": id,
        "providerID": provider_id,
        "name": id,
        "type": "chat",
        "package": package,
        "capabilities": { "tools": false, "input": ["text"], "output": ["text"] },
        "variants": [],
        "time": { "released": 0 },
        "cost": [],
        "status": "active",
        "enabled": true,
        "limit": { "context": 1000, "output": 100 }
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_provider_merges_partial_capabilities_onto_the_catalog_model() {
    let mut harness = start(Input::new(vec![serde_json::json!({
        "providers": {
            "custom": {
                "package": "aisdk:@ai-sdk/openai-compatible",
                "models": { "catalog": { "capabilities": { "input": ["text", "image"], "temperature": true } } }
            }
        }
    })]))
    .await;
    let _seeded = seed(
        &harness,
        "custom",
        "aisdk:@ai-sdk/openai-compatible",
        vec![catalog_model("catalog", "custom", "aisdk:@ai-sdk/openai-compatible")],
    )
    .await;
    harness.register(&["config_provider"]).await;

    let model = harness.services.model.get(&ProviderID("custom".to_owned()), "catalog").await.unwrap().unwrap();
    assert!(!model.capabilities.tools);
    assert_eq!(model.capabilities.input, ["text", "image"]);
    assert_eq!(model.capabilities.output, ["text"]);
    assert_eq!(model.capabilities.temperature, Some(true));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_provider_changes_a_catalog_model_type_and_keeps_it_when_unset() {
    use opencode_contract::generated::{ModelType, ModelTypeString3};

    let mut harness = start(Input::new(vec![serde_json::json!({
        "providers": {
            "custom": {
                "package": "aisdk:@ai-sdk/openai-compatible",
                "models": { "retyped": { "type": "image" }, "kept": { "name": "Kept" } }
            }
        }
    })]))
    .await;
    let mut retyped = catalog_model("retyped", "custom", "aisdk:@ai-sdk/openai-compatible");
    let mut kept = catalog_model("kept", "custom", "aisdk:@ai-sdk/openai-compatible");
    kept["type"] = serde_json::json!("decision");
    retyped["type"] = serde_json::json!("chat");
    let _seeded = seed(&harness, "custom", "aisdk:@ai-sdk/openai-compatible", vec![retyped, kept]).await;
    harness.register(&["config_provider"]).await;

    assert!(harness.services.model.available(None).await.unwrap().is_empty());
    let image = harness.services.model.available(Some(&[ModelType::String3(ModelTypeString3::Image)])).await.unwrap();
    assert_eq!(image.iter().map(|model| model.id.as_str()).collect::<Vec<_>>(), ["retyped"]);
    let decision = harness
        .services
        .model
        .available(Some(&[ModelType::String2(opencode_contract::generated::ModelTypeString2::Decision)]))
        .await
        .unwrap();
    assert_eq!(
        decision.iter().map(|model| (model.id.as_str(), model.name.as_str())).collect::<Vec<_>>(),
        [("kept", "Kept")]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_provider_inherits_the_provider_compaction_setting() {
    use opencode_contract::codec::encode_json;

    let mut harness = start(Input::new(vec![serde_json::json!({
        "providers": {
            "custom": {
                "package": "aisdk:@ai-sdk/openai-compatible",
                "settings": { "compaction": { "type": "native" } },
                "models": {
                    "native": {},
                    "local": {
                        "settings": { "compaction": { "type": "summary" } },
                        "package": "@opencode/ai/providers/openai/chat"
                    }
                }
            }
        }
    })]))
    .await;
    harness.register(&["config_provider"]).await;

    let provider = harness.services.provider.get(&ProviderID("custom".to_owned())).await.unwrap().unwrap();
    let settings = provider.settings.expect("the provider settings");
    let compaction = settings.compaction.expect("the compaction setting");
    assert_eq!(json_value(&encode_json(&compaction)), serde_json::json!({ "type": "native" }));

    let models = harness.services.model.all().await.unwrap();
    let settings = |id: &str| {
        models
            .iter()
            .find(|model| model.id == id)
            .and_then(|model| model.settings.clone())
            .and_then(|settings| settings.compaction)
    };
    assert_eq!(json_value(&encode_json(&settings("native").unwrap())), serde_json::json!({ "type": "native" }));
    assert_eq!(json_value(&encode_json(&settings("local").unwrap())), serde_json::json!({ "type": "summary" }));
}

pub fn json_value(encoded: &str) -> Value {
    serde_json::from_str(encoded).expect("the encoding is JSON")
}
