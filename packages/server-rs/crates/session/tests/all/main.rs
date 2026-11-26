mod corpus;
mod instructions;
mod model_request;
mod publish_llm_event;
mod retry;
mod runner;
mod search_index;
mod store;
mod usage;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::future::BoxFuture;
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
use opencode_fs::access::{Access, Authorizer};
use opencode_fs::formatter::Formatter;
use opencode_fs::mutation::Mutation;
use opencode_fs::ripgrep::Ripgrep;
use opencode_fs::ripgrep::binary::Binary;
use opencode_fs::write::FileWriter;
use opencode_location::state::Registration;
use opencode_permission::service::{AssertInput, PermissionError};
use opencode_registry::agent::AgentService;
use opencode_registry::config::agent;
use opencode_registry::events::RegistryEvents;
use opencode_registry::skill::SkillService;
use opencode_tool::image::Image;
use opencode_tool::output::OutputStore;
use opencode_tool::tools::read::NearbyInstructions;
use opencode_tool::{LocationFilesParts, LocationToolDeps, LocationTools};

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

pub struct AllowAll;

impl Authorizer for AllowAll {
    fn assert<'a>(&'a self, _input: AssertInput) -> BoxFuture<'a, Result<(), PermissionError>> {
        Box::pin(async { Ok(()) })
    }

    fn allows<'a>(&'a self, _input: AssertInput) -> BoxFuture<'a, Result<bool, PermissionError>> {
        Box::pin(async { Ok(true) })
    }
}

pub struct DenyAll;

impl Authorizer for DenyAll {
    fn assert<'a>(&'a self, input: AssertInput) -> BoxFuture<'a, Result<(), PermissionError>> {
        Box::pin(async move {
            Err(PermissionError::Blocked {
                rules: Vec::new(),
                permission: input.action,
                resources: input.resources,
                reason: None,
            })
        })
    }

    fn allows<'a>(&'a self, _input: AssertInput) -> BoxFuture<'a, Result<bool, PermissionError>> {
        Box::pin(async { Ok(false) })
    }
}

struct NoNearby;

impl NearbyInstructions for NoNearby {
    fn load<'a>(&'a self, _session_id: &'a str, _paths: Vec<String>) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

pub async fn test_tools(
    work: &str,
    root: &Path,
    authorizer: Arc<dyn Authorizer>,
    full_codemode: bool,
) -> Arc<LocationTools> {
    let paths = GlobalPaths::from_vars(|name| match name {
        "HOME" => Some(OsString::from("/home/test")),
        "XDG_DATA_HOME" => Some(root.join("data").into_os_string()),
        "XDG_CACHE_HOME" => Some(root.join("cache").into_os_string()),
        "XDG_CONFIG_HOME" => Some(root.join("config").into_os_string()),
        "XDG_STATE_HOME" => Some(root.join("state").into_os_string()),
        _ => Some(OsString::new()),
    })
    .expect("resolve the global paths");
    let access = Access::new(work, work, "/home/test");
    let mutation = Mutation::new();
    let formatter = Arc::new(Formatter::new(work, work, root.join("bin"), move |name| std::env::var_os(name)));
    let files = LocationFilesParts {
        access: access.clone(),
        mutation: mutation.clone(),
        write: FileWriter::new(access, mutation, authorizer.clone(), formatter.clone()),
        formatter,
    };
    Arc::new(
        LocationTools::new(LocationToolDeps {
            files,
            ripgrep: Arc::new(Ripgrep::new(Binary::new(paths, |name| std::env::var_os(name), Vec::new()))),
            skills: SkillService::new(Arc::new(NoEvents)),
            instructions: Arc::new(NoNearby),
            authorizer,
            output: OutputStore::new(root.join("data")),
            image: Arc::new(Image::new()),
            claude: true,
            full_codemode,
        })
        .await,
    )
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
