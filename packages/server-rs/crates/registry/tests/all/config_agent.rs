use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use opencode_config::entry::{Document, Entry, Origin};
use opencode_config::observer;
use opencode_config::options::{ConfigLocation, ConfigOptions, SystemOption};
use opencode_config::paths::GlobalPaths;
use opencode_config::seams::NoKv;
use opencode_config::service::{ConfigService, ServiceDeps};
use opencode_config::wellknown::WellKnown;
use opencode_contract::codec::decode_json;
use opencode_contract::generated::{
    AgentColor, AgentInfoMode, ConfigEntryDocumentInfo, ModelRef, PermissionEffect, PermissionRule, ProviderID,
};
use opencode_registry::agent::{AgentService, default_info};
use opencode_registry::builtin;
use opencode_registry::config::agent;
use opencode_registry::events::RegistryEvents;

use crate::TempDir;

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<&'static str>>,
}

impl RegistryEvents for Recorder {
    fn updated(&self, r#type: &'static str) {
        self.events.lock().expect("the event lock is not poisoned").push(r#type);
    }
}

fn rule(action: &str, resource: &str, effect: PermissionEffect) -> PermissionRule {
    PermissionRule { action: action.to_string(), resource: resource.to_string(), effect }
}

fn write(path: &Path, content: &str) {
    std::fs::create_dir_all(path.parent().expect("the file has a parent")).expect("create the file directory");
    std::fs::write(path, content).expect("write the file");
}

fn document(json: &str) -> ConfigEntryDocumentInfo {
    decode_json::<ConfigEntryDocumentInfo>(json).expect("the document decodes")
}

#[tokio::test]
async fn loads_directory_and_document_entries() {
    let temp = TempDir::new();
    let root = temp.path().join(".opencode");
    write(
        &root.join("agent/legacy.md"),
        "---\nmodel: openai/gpt-5\ntemperature: 0.2\ntools:\n  write: false\ndescription: Legacy\n---\nLegacy body.\n",
    );
    write(&root.join("agents/team/helper.md"), "Helper body.\n");
    write(
        &root.join("agents/v2.md"),
        "---\nmodel: openai/gpt-5\nvariant: fast\nrequest:\n  headers:\n    x-test: one\npermissions:\n  - action: read\n    resource: src/**\n    effect: allow\n---\nV2 body.\n",
    );
    write(&root.join("modes/plan.md"), "Plan body.\n");
    write(&root.join("agents/disabled.md"), "---\ndisabled: true\n---\nGone.\n");
    write(&root.join("agents/empty.md"), "");
    write(&root.join("agents/.opencode/node_modules/ignored.md"), "Ignored body.\n");

    let extra = document(r#"{"agents":{"document_agent":{"system":"Document system"}}}"#);
    let entries = vec![
        Entry::Directory { path: root.clone() },
        Entry::Document(Document { path: None, info: extra, origin: Origin::File }),
    ];

    let home = temp.path().join("home");
    std::fs::create_dir_all(&home).expect("create the home directory");
    let service = AgentService::new(Arc::new(Recorder::default()));
    let _builtins = service.transform(builtin::agent::agent).await;
    let _plan = service
        .transform({
            let home = home.clone();
            move |editor| builtin::agent::plan(editor, &home)
        })
        .await;
    let _config = service
        .transform({
            let home = home.clone();
            move |editor| agent::apply(&agent::load(&entries), &home, editor)
        })
        .await;

    let agents = service.list().await.expect("the state is readable");
    let ids: Vec<&str> = agents.iter().map(|info| info.id.0.as_str()).collect();
    assert_eq!(
        ids,
        [
            "build",
            "general",
            "explore",
            "compaction",
            "title",
            "summary",
            "plan",
            "legacy",
            "team/helper",
            "v2",
            "document_agent"
        ]
    );

    let legacy = agents.iter().find(|info| info.id.0 == "legacy").expect("legacy is listed");
    assert_eq!(legacy.system.as_deref(), Some("Legacy body."));
    assert_eq!(
        legacy.model,
        Some(ModelRef { id: "gpt-5".to_string(), provider_id: ProviderID("openai".to_string()), variant: None })
    );
    assert_eq!(legacy.request.body.0, [("temperature".to_string(), serde_json::json!(0.2))]);
    let mut expected = default_info("legacy").permissions;
    expected.push(rule("edit", "*", PermissionEffect::Deny));
    assert_eq!(legacy.permissions, expected);

    let helper = agents.iter().find(|info| info.id.0 == "team/helper").expect("the nested agent is listed");
    assert_eq!(helper.system.as_deref(), Some("Helper body."));

    let v2 = agents.iter().find(|info| info.id.0 == "v2").expect("the v2 agent is listed");
    assert_eq!(
        v2.model,
        Some(ModelRef {
            id: "gpt-5".to_string(),
            provider_id: ProviderID("openai".to_string()),
            variant: Some("fast".to_string()),
        })
    );
    assert_eq!(v2.request.headers.0, [("x-test".to_string(), "one".to_string())]);
    assert!(v2.permissions.contains(&rule("read", "src/**", PermissionEffect::Allow)));

    let plan = agents.iter().find(|info| info.id.0 == "plan").expect("plan is listed");
    assert_eq!(plan.system.as_deref(), Some("Plan body."));
    assert_eq!(plan.mode, AgentInfoMode::Primary);

    let document_agent =
        agents.iter().find(|info| info.id.0 == "document_agent").expect("the document agent is listed");
    assert_eq!(document_agent.system.as_deref(), Some("Document system"));

    assert!(agents.iter().all(|info| info.id.0 != "disabled"));
    assert!(agents.iter().all(|info| info.id.0 != "empty"));
    assert!(agents.iter().all(|info| !info.id.0.contains("ignored")));
}

#[tokio::test]
async fn applies_document_agents_permissions_and_defaults() {
    let temp = TempDir::new();
    let home = temp.path().join("home");
    std::fs::create_dir_all(&home).expect("create the home directory");
    let home_text = home.to_string_lossy().into_owned();

    let documents = vec![
        document(
            r##"{
                "default_agent": "alpha",
                "permissions": [
                    {"action": "read", "resource": "~/.secrets", "effect": "ask"},
                    {"action": "shell", "resource": "$HOME/private/**", "effect": "deny"},
                    {"action": "external_directory", "resource": "$HOME", "effect": "allow"},
                    {"action": "edit", "resource": "~/src/**", "effect": "allow"}
                ],
                "agents": {
                    "build": {"request": {"headers": {"x-one": "first", "x-two": "keep"}}},
                    "alpha": {
                        "description": "Alpha",
                        "model": "anthropic/claude-sonnet-4#thinking",
                        "request": {"headers": {"x-one": "first"}, "body": {"temperature": 1}},
                        "permissions": [{"action": "edit", "resource": "~/alpha/**", "effect": "allow"}]
                    },
                    "beta": {
                        "model": {"providerID": "openai", "model": "gpt-5", "variant": "fast"},
                        "hidden": true,
                        "steps": 3,
                        "color": "#aabbcc",
                        "mode": "subagent"
                    },
                    "explore": {"disabled": true}
                }
            }"##,
        ),
        document(
            r#"{
                "default_agent": "gamma",
                "permissions": [{"action": "read", "resource": "$HOME/metrics", "effect": "allow"}],
                "agents": {
                    "alpha": {
                        "request": {"headers": {"x-two": "second", "x-three": "new"}, "body": {"top_p": 5}}
                    },
                    "gamma": {"system": "Gamma system", "mode": "all"}
                }
            }"#,
        ),
    ];

    let service = AgentService::new(Arc::new(Recorder::default()));
    let _builtins = service.transform(builtin::agent::agent).await;
    let _config = service
        .transform({
            let documents = documents.clone();
            let home = home.clone();
            move |editor| agent::apply(&documents, &home, editor)
        })
        .await;

    let global = vec![
        rule("read", &format!("{home_text}/.secrets"), PermissionEffect::Ask),
        rule("shell", "$HOME/private/**", PermissionEffect::Deny),
        rule("external_directory", &home_text, PermissionEffect::Allow),
        rule("edit", &format!("{home_text}/src/**"), PermissionEffect::Allow),
        rule("read", &format!("{home_text}/metrics"), PermissionEffect::Allow),
    ];

    let build = service.get("build").await.expect("the state is readable").expect("build exists");
    let mut expected = default_info("build").permissions;
    expected.push(rule("question", "*", PermissionEffect::Allow));
    expected.extend(global.clone());
    assert_eq!(build.permissions, expected);

    let alpha = service.get("alpha").await.expect("the state is readable").expect("alpha exists");
    let mut expected = default_info("alpha").permissions;
    expected.extend(global.clone());
    expected.push(rule("edit", &format!("{home_text}/alpha/**"), PermissionEffect::Allow));
    assert_eq!(alpha.permissions, expected);
    assert_eq!(alpha.description.as_deref(), Some("Alpha"));
    assert_eq!(
        alpha.model,
        Some(ModelRef {
            id: "claude-sonnet-4".to_string(),
            provider_id: ProviderID("anthropic".to_string()),
            variant: Some("thinking".to_string()),
        })
    );
    assert_eq!(
        alpha.request.headers.0,
        [
            ("x-one".to_string(), "first".to_string()),
            ("x-two".to_string(), "second".to_string()),
            ("x-three".to_string(), "new".to_string()),
        ]
    );
    assert_eq!(
        alpha.request.body.0,
        [("temperature".to_string(), serde_json::json!(1)), ("top_p".to_string(), serde_json::json!(5))]
    );

    let beta = service.get("beta").await.expect("the state is readable").expect("beta exists");
    assert_eq!(
        beta.model,
        Some(ModelRef {
            id: "gpt-5".to_string(),
            provider_id: ProviderID("openai".to_string()),
            variant: Some("fast".to_string()),
        })
    );
    assert!(beta.hidden);
    assert_eq!(beta.steps, Some(3.0));
    assert_eq!(beta.color, Some(AgentColor("#aabbcc".to_string())));
    assert_eq!(beta.mode, AgentInfoMode::Subagent);

    let gamma = service.get("gamma").await.expect("the state is readable").expect("gamma exists");
    assert_eq!(gamma.system.as_deref(), Some("Gamma system"));
    assert_eq!(gamma.mode, AgentInfoMode::All);

    assert!(service.get("explore").await.expect("the state is readable").is_none());
    let selected = service.select(None).await.expect("the state is readable");
    assert_eq!(selected.id, "gamma");
}

struct Workspace {
    _temp: TempDir,
    home: PathBuf,
    config: PathBuf,
    project: PathBuf,
}

fn workspace() -> Workspace {
    let temp = TempDir::new();
    let root = std::fs::canonicalize(temp.path()).expect("the temp directory resolves");
    let project = root.join("project");
    let home = root.join("home");
    let config = root.join("global");
    for directory in [&project, &home, &config] {
        std::fs::create_dir_all(directory).expect("create the workspace directory");
    }
    Workspace { _temp: temp, home, config, project }
}

async fn start(workspace: &Workspace) -> ConfigService {
    let paths = GlobalPaths::from_vars(|name| match name {
        "HOME" => Some(workspace.home.clone().into_os_string()),
        "OPENCODE_CONFIG_DIR" => Some(workspace.config.clone().into_os_string()),
        _ => None,
    })
    .expect("the global paths resolve");
    let options = ConfigOptions {
        project: Some(true),
        global: Some(false),
        system: Some(SystemOption::Disabled),
        claude: Some(false),
        file: None,
        content: None,
    };
    let location = ConfigLocation { directory: workspace.project.clone(), project_id: "test".to_owned() };
    let deps = ServiceDeps {
        credentials: Arc::new(()),
        wellknown: Arc::new(WellKnown::new(Arc::new(NoKv))),
        channel: "local".to_owned(),
        flag_overrides: None,
    };
    ConfigService::start(options, location, paths, deps).await
}

async fn wait_for_system(agents: &AgentService, id: &str, system: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let info = agents.get(id).await.expect("the agent state is readable");
            if info.and_then(|info| info.system).as_deref() == Some(system) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the agent system updates");
}

#[tokio::test]
async fn register_reloads_agent_files_when_they_change() {
    let workspace = workspace();
    std::fs::create_dir_all(workspace.project.join(".opencode/agent")).expect("create the agent directory");
    let config = start(&workspace).await;
    let agents = AgentService::new(Arc::new(Recorder::default()));
    let _builtins = agents.transform(builtin::agent::agent).await;
    let _handle = agent::register(&config, &agents, &workspace.home).await;

    let file = workspace.project.join(".opencode/agent/reviewer.md");
    write(&file, "---\ndescription: Reviewer\n---\nFirst system.\n");
    wait_for_system(&agents, "reviewer", "First system.").await;

    write(&file, "---\ndescription: Reviewer\n---\nSecond system.\n");
    wait_for_system(&agents, "reviewer", "Second system.").await;
}

async fn wait_for_count(count: &AtomicUsize, target: usize) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while count.load(Ordering::SeqCst) < target {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the observer applies the change");
}

#[tokio::test]
async fn observe_sources_coalesces_rapid_agent_writes() {
    let workspace = workspace();
    std::fs::create_dir_all(workspace.project.join(".opencode/agent")).expect("create the agent directory");
    let config = start(&workspace).await;
    let count = Arc::new(AtomicUsize::new(0));
    let applied = count.clone();
    let _handle = observer::observe_sources(&config, &["agent"], agent::load, move |_documents| {
        applied.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(count.load(Ordering::SeqCst), 1);

    let file = workspace.project.join(".opencode/agent/probe.md");
    for _ in 0..3 {
        write(&file, "Probe body.\n");
    }
    wait_for_count(&count, 2).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(count.load(Ordering::SeqCst), 2);

    write(&workspace.project.join("notes.txt"), "notes\n");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(count.load(Ordering::SeqCst), 2);
}
