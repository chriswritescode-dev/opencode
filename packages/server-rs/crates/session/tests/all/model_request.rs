use std::path::Path;
use std::sync::Arc;

use opencode_ai::{ContentPart, ExecutionContext, MapEnv, Message, StreamOptions, SystemPart};
use opencode_bus::{Bus, BusOptions, Definition, PublishOptions};
use opencode_catalog::Catalog;
use opencode_config::entry::{Document, Entry, Origin};
use opencode_contract::codec::decode_json;
use opencode_contract::generated::{ConfigEntryDocumentInfo, SessionInboxDelivery};
use opencode_location::environment::local::LocalEnvironment;
use opencode_location::project::ProjectService;
use opencode_session::agent::Agents;
use opencode_session::context::{self, LoadInput};
use opencode_session::instruction_state;
use opencode_session::instructions::{self, BuiltinsInput, List};
use opencode_session::model_request::{self, AppInfo};
use opencode_session::{ProjectionSettings, PromptInput, SessionProjector, SessionStore, Sessions};
use opencode_storage::{Database, DatabaseLocation};
use opencode_util::time::Zone;
use serde_json::{Value, json};

use crate::TempDir;

const FIXED_NOW: i64 = 1_800_000_000_000;

struct Setup {
    dir: TempDir,
    database: Arc<Database>,
    bus: Arc<Bus>,
    store: SessionStore,
    catalog: Catalog,
    agents: Agents,
    list: List,
    work: String,
    sessions: Sessions,
}

impl Setup {
    fn builtins(&self) -> BuiltinsInput {
        builtins_input(&self.work, self.dir.path())
    }
}

fn app() -> AppInfo {
    AppInfo { name: "opencode".to_owned(), channel: "local".to_owned(), version: "0.0.0-test".to_owned() }
}

fn system_text(parts: &[SystemPart]) -> String {
    parts
        .iter()
        .map(|part| match part {
            SystemPart::Text { text, .. } => text.as_str(),
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn system_message_texts(messages: &[Message]) -> Vec<&str> {
    messages
        .iter()
        .filter_map(|message| {
            let Message::System(system) = message else { return None };
            system.content.iter().find_map(|part| match part {
                ContentPart::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
        })
        .collect()
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

fn setup(build_system: Option<&str>) -> Setup {
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
    let agents = Agents::from_entries(std::slice::from_ref(&entry), "/home/test");

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

    Setup { dir, database, bus, store, catalog, agents, list, work, sessions }
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

#[test]
fn prepared_request_matches_the_typescript_body() {
    let setup = setup(None);
    instruction_state::prepare(&setup.database, &setup.bus, &setup.list, "ses_1")
        .expect("prepare the builtin instructions");
    let env = MapEnv::default();
    let loaded = context::load_with(
        &LoadInput {
            database: &setup.database,
            store: &setup.store,
            catalog: &setup.catalog,
            agents: &setup.agents,
            env: &env,
            builtins: setup.builtins(),
        },
        "ses_1",
        setup.list,
    )
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

#[test]
fn configured_agent_system_replaces_the_default_prompt() {
    let setup = setup(Some("You are a test agent."));
    instruction_state::prepare(&setup.database, &setup.bus, &setup.list, "ses_1")
        .expect("prepare the builtin instructions");
    let env = MapEnv::default();
    let loaded = context::load_with(
        &LoadInput {
            database: &setup.database,
            store: &setup.store,
            catalog: &setup.catalog,
            agents: &setup.agents,
            env: &env,
            builtins: setup.builtins(),
        },
        "ses_1",
        setup.list,
    )
    .expect("load the session context");
    let request = model_request::prepare(&loaded, &app());
    let SystemPart::Text { text, .. } = request.system.first().expect("the request carries a system prompt");
    assert_eq!(text.as_str(), "You are a test agent.");
}

const PLAN_SELECTED: &str = "You are in Plan mode. Discuss the plan with the user directly in the conversation.";
const PLAN_UNSELECTED: &str = "You are no longer the Plan agent, so you are NO LONGER in Plan mode.";

#[test]
fn plan_agent_instructions_reach_the_request_and_leave_once() {
    let setup = setup(None);
    setup
        .database
        .read(|connection| Ok(connection.execute("UPDATE session_v2 SET agent = 'plan' WHERE id = 'ses_1'", [])?))
        .expect("select the plan agent");
    let env = MapEnv::default();
    let input = LoadInput {
        database: &setup.database,
        store: &setup.store,
        catalog: &setup.catalog,
        agents: &setup.agents,
        env: &env,
        builtins: setup.builtins(),
    };

    let plan_list = context::instruction_list(&setup.store, &setup.agents, setup.builtins(), "ses_1")
        .expect("build the plan instruction list");
    instruction_state::prepare(&setup.database, &setup.bus, &plan_list, "ses_1")
        .expect("prepare the plan instructions");
    let plan = context::load_with(&input, "ses_1", plan_list).expect("load the plan context");
    let plan_system = system_text(&model_request::prepare(&plan, &app()).system);
    assert!(plan_system.contains(PLAN_SELECTED), "the plan instructions reach the system prompt");
    assert!(plan_system.contains("/home/test/.opencode/plan"), "the plan directory reaches the system prompt");

    setup.sessions.switch_agent("ses_1", "build").expect("switch to the build agent");
    let build_list = context::instruction_list(&setup.store, &setup.agents, setup.builtins(), "ses_1")
        .expect("build the build instruction list");
    instruction_state::prepare(&setup.database, &setup.bus, &build_list, "ses_1")
        .expect("prepare the build instructions");
    let build = context::load_with(&input, "ses_1", build_list).expect("load the build context");
    let build_request = model_request::prepare(&build, &app());
    assert_eq!(
        system_message_texts(&build_request.messages).into_iter().filter(|text| text.contains(PLAN_UNSELECTED)).count(),
        1,
        "the plan leave text is sent exactly once"
    );

    let repeated = context::instruction_list(&setup.store, &setup.agents, setup.builtins(), "ses_1")
        .expect("rebuild the build instruction list");
    instruction_state::prepare(&setup.database, &setup.bus, &repeated, "ses_1")
        .expect("prepare the unchanged build instructions");
    let build = context::load_with(&input, "ses_1", repeated).expect("reload the build context");
    assert_eq!(
        system_message_texts(&model_request::prepare(&build, &app()).messages)
            .into_iter()
            .filter(|text| text.contains(PLAN_UNSELECTED))
            .count(),
        1,
        "the plan leave text is not repeated on the next step"
    );
}
