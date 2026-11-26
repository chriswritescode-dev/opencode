mod corpus;
mod instructions;
mod model_request;
mod retry;
mod runner;
mod search_index;
mod store;
mod usage;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use opencode_ai::MapEnv;
use opencode_bus::{Bus, BusOptions};
use opencode_catalog::builtin::{self, Builtin, Services};
use opencode_catalog::credential::CredentialService;
use opencode_catalog::integration::IntegrationService;
use opencode_catalog::model::ModelService;
use opencode_catalog::models_dev::{ModelsDevService, Options};
use opencode_catalog::provider::ProviderService;
use opencode_catalog::resolver::{NoBridge, Resolver};
use opencode_config::entry::Entry;
use opencode_config::options::{ConfigLocation, ConfigOptions, SystemOption};
use opencode_config::paths::GlobalPaths;
use opencode_config::seams::NoKv;
use opencode_config::service::{ConfigService, ServiceDeps};
use opencode_config::wellknown::WellKnown;
use opencode_contract::generated::{AbsolutePath, LocationRef};
use opencode_location::state::Registration;
use opencode_registry::agent::AgentService;
use opencode_registry::config::agent;
use opencode_registry::events::RegistryEvents;

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "opencode-session-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("create the temp directory");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

struct NoEvents;

impl RegistryEvents for NoEvents {
    fn updated(&self, _type: &'static str) {}
}

struct TestAgents {
    service: AgentService,
    _registration: Registration,
}

async fn test_agents(entries: &[Entry], home: &str) -> TestAgents {
    let service = AgentService::new(Arc::new(NoEvents));
    let documents = agent::load(entries);
    let home = PathBuf::from(home);
    let registration = service
        .transform(move |editor| {
            opencode_registry::builtin::agent::agent(editor);
            opencode_registry::builtin::agent::plan(editor, &home);
            agent::apply(&documents, &home, editor);
        })
        .await;
    TestAgents { service, _registration: registration }
}

struct TestCatalog {
    models: Arc<ModelService>,
    resolver: Arc<Resolver>,
    _builtin: Builtin,
    _temp: TempDir,
}

async fn test_catalog(config: &serde_json::Value, env: MapEnv) -> TestCatalog {
    let temp = TempDir::new();
    let (database, _) = opencode_storage::Database::open(opencode_storage::DatabaseLocation::Memory, temp.path())
        .expect("open the catalog database");
    let database = Arc::new(database);
    let bus = Bus::new(database.clone(), BusOptions::default());
    let credentials = CredentialService::new(database.clone(), bus.clone(), temp.path().join("locks"));
    let wellknown = Arc::new(WellKnown::new(Arc::new(NoKv)));
    let home = temp.path().to_path_buf();
    let paths = GlobalPaths::from_vars(|name| (name == "HOME").then(|| home.clone().into_os_string()))
        .expect("the catalog paths resolve");
    let config = ConfigService::start(
        ConfigOptions {
            project: Some(false),
            global: Some(false),
            system: Some(SystemOption::Disabled),
            claude: Some(false),
            file: None,
            content: Some(config.to_string()),
        },
        ConfigLocation { directory: temp.path().to_path_buf(), project_id: "test".to_owned() },
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
        LocationRef { directory: AbsolutePath(temp.path().to_string_lossy().into_owned()), workspace_id: None };
    let config_env: opencode_catalog::integration::ConfigEnv = Arc::new(|_| None);
    let integration = IntegrationService::new(
        credentials.clone(),
        bus.clone(),
        location.clone(),
        config_env.clone(),
        Arc::new(Vec::new()),
    );
    let provider = ProviderService::new(integration.clone(), bus.clone(), location.clone());
    let model = ModelService::new(provider.clone(), bus.clone(), location, config_env);
    let models_dev = ModelsDevService::new(
        database,
        bus.clone(),
        Options { url: None, file: None, fetch: Some(false), snapshot: Some(false) },
        "opencode/test".to_owned(),
    );
    let services = Services {
        bus,
        config,
        credentials,
        models_dev,
        wellknown,
        integration: integration.clone(),
        provider: provider.clone(),
        model: model.clone(),
    };
    let mut registered = Builtin::default();
    builtin::config_provider::register(&services, &mut registered).await;
    let resolver = Resolver::new(provider, model.clone(), integration, Arc::new(env), Arc::new(NoBridge));
    TestCatalog { models: model, resolver, _builtin: registered, _temp: temp }
}
