use std::path::Path;
use std::sync::Arc;

use opencode_ai::{ExecutionContext, MapEnv, StreamOptions, SystemPart};
use opencode_bus::{Bus, BusOptions, Definition, PublishOptions};
use opencode_catalog::legacy::Catalog;
use opencode_config::entry::{Document, Entry, Origin};
use opencode_contract::codec::decode_json;
use opencode_contract::generated::{ConfigEntryDocumentInfo, SessionInboxDelivery};
use opencode_location::environment::local::LocalEnvironment;
use opencode_location::project::ProjectService;
use opencode_session::context::{self, LoadInput};
use opencode_session::instruction_state;
use opencode_session::instructions::{self, BuiltinsInput, List};
use opencode_session::model_request::{self, AppInfo};
use opencode_session::{ProjectionSettings, PromptInput, SessionProjector, SessionStore, Sessions};
use opencode_storage::{Database, DatabaseLocation};
use opencode_util::time::Zone;
use serde_json::{Value, json};

use crate::{TempDir, TestAgents, test_agents};

const FIXED_NOW: i64 = 1_800_000_000_000;

struct Setup {
    dir: TempDir,
    database: Arc<Database>,
    bus: Arc<Bus>,
    store: SessionStore,
    catalog: Catalog,
    agents: TestAgents,
    list: List,
    work: String,
}

impl Setup {
    fn builtins(&self) -> BuiltinsInput {
        builtins_input(&self.work, self.dir.path())
    }
}

fn app() -> AppInfo {
    AppInfo { name: "opencode".to_owned(), channel: "local".to_owned(), version: "0.0.0-test".to_owned() }
}

fn builtins_input(work: &str, root: &Path) -> BuiltinsInput {
    BuiltinsInput {
        directory: work.to_owned(),
        workspace_root: work.to_owned(),
        vcs: None,
        tmp: root.join("tmp/opencode").to_string_lossy().into_owned(),
    }
}

fn config_value(build_system: Option<&str>) -> Value {
    let mut agents = json!({ "title": { "disabled": true } });
    if let Some(system) = build_system {
        agents["build"] = json!({ "system": system });
    }
    json!({
        "model": "anthropic-test/test-model",
        "agents": agents,
        "providers": {
            "anthropic-test": {
                "name": "Anthropic Test",
                "package": "@ai-sdk/anthropic",
                "settings": { "apiKey": "parity", "baseURL": "http://127.0.0.1:9/v1" },
                "models": {
                    "test-model": {
                        "name": "Test Model",
                        "capabilities": { "tools": true, "input": ["text"], "output": ["text"] },
                        "cost": { "input": 0, "output": 0 },
                        "limit": { "context": 100000, "output": 10000 }
                    }
                }
            }
        }
    })
}

async fn setup(build_system: Option<&str>) -> Setup {
    let dir = TempDir::new();
    let work_path = dir.path().join("work");
    std::fs::create_dir_all(&work_path).expect("create the work directory");
    let work = work_path.to_string_lossy().into_owned();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    let database = Arc::new(database);
    let bus = Bus::new(
        database.clone(),
        BusOptions { persist: true, now: Some(Arc::new(|| FIXED_NOW)), ..Default::default() },
    );
    SessionProjector::register(&bus, Arc::new(ProjectionSettings::default));
    database
        .read(|connection| {
            connection.execute(
                "INSERT INTO project (id, worktree, sandboxes, time_created, time_updated) \
                 VALUES ('prj_1', ?1, '[]', 1, 2)",
                [work.as_str()],
            )?;
            connection.execute(
                "INSERT INTO session_v2 (id, project_id, slug, directory, version, time_created, time_updated) \
                 VALUES ('ses_1', 'prj_1', 'slug-1', ?1, '1', 1000, 1000)",
                [work.as_str()],
            )?;
            Ok(())
        })
        .expect("seed the session");

    let store = SessionStore::new(database.clone());
    let list = instructions::builtins_at(builtins_input(&work, dir.path()), FIXED_NOW)
        .expect("build the builtin instructions");

    let value = config_value(build_system);
    let entry = Entry::Document(Document {
        path: None,
        info: decode_json::<ConfigEntryDocumentInfo>(&value.to_string()).expect("decode the config document"),
        origin: Origin::File,
    });
    let catalog = Catalog::from_entries(std::slice::from_ref(&entry));
    let agents = test_agents(std::slice::from_ref(&entry), "/home/test").await;

    let sessions = Sessions::new(
        store.clone(),
        bus.clone(),
        Arc::new(ProjectService::new(database.clone())),
        Arc::new(LocalEnvironment::new()),
        "1".to_owned(),
    );
    let inbox = sessions
        .prompt(
            "ses_1",
            PromptInput {
                id: None,
                text: "say hi".to_owned(),
                agents: None,
                metadata: None,
                delivery: SessionInboxDelivery::Steer,
                resume: false,
            },
        )
        .expect("admit the prompt");
    let definition = Definition::latest_durable("session.inbox.delivered").expect("session.inbox.delivered is durable");
    bus.publish(&definition, json!({ "sessionID": "ses_1", "inboxID": inbox.id.0 }), PublishOptions::default())
        .expect("deliver the prompt");

    Setup { dir, database, bus, store, catalog, agents, list, work }
}

fn substitute(value: &mut Value, root: &str, date: &str, platform: &str) {
    match value {
        Value::Array(items) => items.iter_mut().for_each(|item| substitute(item, root, date, platform)),
        Value::Object(entries) => entries.values_mut().for_each(|item| substitute(item, root, date, platform)),
        Value::String(text) => {
            *text = text
                .replace("<root>", root)
                .replace("Today's date: Wed Nov 25 2026", &format!("Today's date: {date}"))
                .replace("Platform: linux", &format!("Platform: {platform}"));
        }
        _ => {}
    }
}

#[tokio::test]
async fn prepared_request_matches_the_typescript_body() {
    let setup = setup(None).await;
    instruction_state::prepare(&setup.database, &setup.bus, &setup.list, "ses_1")
        .expect("prepare the builtin instructions");
    let env = MapEnv::default();
    let loaded = context::load_with(
        &LoadInput {
            database: &setup.database,
            store: &setup.store,
            catalog: &setup.catalog,
            agents: &setup.agents.service,
            env: &env,
            builtins: setup.builtins(),
        },
        "ses_1",
        setup.list,
    )
    .await
    .expect("load the session context");
    let request = model_request::prepare(&loaded, &app());
    let compiled = opencode_ai::route::compile::compile(
        &loaded.runtime,
        request,
        &StreamOptions::default(),
        &ExecutionContext::default(),
    )
    .expect("compile the model request");
    let actual = compiled.body;

    let root = setup.dir.path().to_string_lossy().into_owned();
    let date = Zone::Local.at(FIXED_NOW).expect("the fixed instant has a local date").date_string();
    let platform = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    let mut expected = serde_json::from_str::<Value>(include_str!("../fixtures/prompt-text-only-request.json"))
        .expect("parse the captured request");
    substitute(&mut expected, &root, &date, platform);
    let tools = expected.as_object_mut().expect("the captured request is an object").remove("tools");
    assert!(tools.is_some(), "the captured request carries tools");
    assert!(actual.get("tools").is_none(), "the text-only request omits tools");
    assert_eq!(actual, expected);
}

#[tokio::test]
async fn configured_agent_system_replaces_the_default_prompt() {
    let setup = setup(Some("You are a test agent.")).await;
    instruction_state::prepare(&setup.database, &setup.bus, &setup.list, "ses_1")
        .expect("prepare the builtin instructions");
    let env = MapEnv::default();
    let loaded = context::load_with(
        &LoadInput {
            database: &setup.database,
            store: &setup.store,
            catalog: &setup.catalog,
            agents: &setup.agents.service,
            env: &env,
            builtins: setup.builtins(),
        },
        "ses_1",
        setup.list,
    )
    .await
    .expect("load the session context");
    let request = model_request::prepare(&loaded, &app());
    let SystemPart::Text { text, .. } = request.system.first().expect("the request carries a system prompt");
    assert_eq!(text.as_str(), "You are a test agent.");
}
