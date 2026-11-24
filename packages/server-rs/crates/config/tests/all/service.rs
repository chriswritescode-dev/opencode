use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use opencode_config::discovery::{Root, Sources};
use opencode_config::entry::{Entry, Origin, latest};
use opencode_config::options::{ConfigLocation, ConfigOptions, SystemOption};
use opencode_config::parse::parse_info;
use opencode_config::paths::GlobalPaths;
use opencode_config::seams::NoKv;
use opencode_config::service::{ConfigService, ConfigUpdateError, IntegrationDocument, ServiceDeps};
use opencode_config::watch::{WatchUpdate, plan};
use opencode_config::wellknown::WellKnown;
use opencode_contract::generated::requests::ConfigPatch;
use opencode_util::temp::TempDir;
use serde_json::{Value, json};
use tokio::sync::broadcast::{self, Receiver};

use crate::support::area;

struct Workspace {
    _temp: TempDir,
    home: PathBuf,
    config: PathBuf,
    project: PathBuf,
}

fn workspace() -> Workspace {
    let temp = TempDir::new().unwrap();
    let root = opencode_util::path::realpath(temp.path()).unwrap();
    let project = root.join("project");
    let home = root.join("home");
    let config = root.join("global");
    for directory in [&project, &home, &config] {
        std::fs::create_dir_all(directory).unwrap();
    }
    Workspace { _temp: temp, home, config, project }
}

async fn start(workspace: &Workspace) -> ConfigService {
    let paths = GlobalPaths::from_vars(|name| match name {
        "HOME" => Some(workspace.home.clone().into_os_string()),
        "OPENCODE_CONFIG_DIR" => Some(workspace.config.clone().into_os_string()),
        _ => None,
    })
    .unwrap();
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

#[tokio::test]
async fn writing_a_project_config_publishes_one_update() {
    let workspace = workspace();
    let service = start(&workspace).await;
    let mut updated = service.subscribe_updated();

    std::fs::write(workspace.project.join("opencode.json"), r#"{"shell":"initial"}"#).unwrap();
    wait_updated(&mut updated).await;
    assert_eq!(shell(&service), Some("initial".to_owned()));
    assert_no_update(&mut updated).await;
}

#[tokio::test]
async fn rewriting_identical_content_publishes_no_update() {
    let workspace = workspace();
    std::fs::write(workspace.project.join("opencode.json"), r#"{"shell":"same"}"#).unwrap();
    let service = start(&workspace).await;
    assert_eq!(shell(&service), Some("same".to_owned()));
    let mut updated = service.subscribe_updated();

    std::fs::write(workspace.project.join("opencode.json"), r#"{"shell":"same"}"#).unwrap();
    assert_no_update(&mut updated).await;
    assert_eq!(shell(&service), Some("same".to_owned()));
}

#[tokio::test]
async fn creating_a_missing_project_directory_is_picked_up() {
    let workspace = workspace();
    let service = start(&workspace).await;
    let mut updated = service.subscribe_updated();
    let root = workspace.project.join(".opencode");

    std::fs::create_dir(&root).unwrap();
    wait_updated(&mut updated).await;
    assert!(
        service.entries().iter().any(|entry| matches!(entry, Entry::Directory { path } if path == &root)),
        "the new .opencode directory is loaded"
    );
}

#[tokio::test]
async fn a_referenced_file_change_reloads() {
    let workspace = workspace();
    let reference = workspace.project.join(".opencode/agent/prompt.md");
    std::fs::create_dir_all(reference.parent().unwrap()).unwrap();
    std::fs::write(&reference, "alpha").unwrap();
    std::fs::write(workspace.project.join("opencode.json"), r#"{"instructions":["{file:.opencode/agent/prompt.md}"]}"#)
        .unwrap();
    let service = start(&workspace).await;
    assert_eq!(instructions(&service), Some(vec!["alpha".to_owned()]));
    let mut updated = service.subscribe_updated();

    std::fs::write(&reference, "beta").unwrap();
    wait_updated(&mut updated).await;
    assert_eq!(instructions(&service), Some(vec!["beta".to_owned()]));
}

#[tokio::test]
async fn changes_report_writes_under_a_config_directory() {
    let workspace = workspace();
    std::fs::create_dir_all(workspace.project.join(".opencode/agent")).unwrap();
    let service = start(&workspace).await;
    let mut changes = service.changes();
    let file = workspace.project.join(".opencode/agent/probe.md");

    std::fs::write(&file, "probe").unwrap();
    let update = wait_change(&mut changes).await;
    assert_eq!(update.path, file);
}

#[tokio::test]
async fn an_idle_service_reports_no_changes() {
    let workspace = workspace();
    let file = workspace.project.join("opencode.json");
    std::fs::write(&file, r#"{"shell":"idle"}"#).unwrap();
    let service = start(&workspace).await;
    let mut changes = service.changes();

    assert!(
        tokio::time::timeout(Duration::from_millis(500), changes.recv()).await.is_err(),
        "an idle service reports no changes"
    );

    std::fs::write(&file, r#"{"shell":"written"}"#).unwrap();
    let update = wait_change(&mut changes).await;
    assert_eq!(update.path, file);
}

#[tokio::test]
async fn an_equal_integration_document_does_not_reload() {
    let workspace = workspace();
    let service = start(&workspace).await;
    let document = IntegrationDocument {
        connection: json!({ "type": "credential", "id": "connection" }),
        organization: None,
        info: parse_info(r#"{"shell":"integrated"}"#, "integration").info.unwrap(),
    };
    let mut updated = service.subscribe_updated();
    service.integration("first", Some(document.clone())).await;
    wait_updated(&mut updated).await;
    assert_eq!(shell(&service), Some("integrated".to_owned()));

    service.integration("first", Some(document)).await;
    assert_no_update(&mut updated).await;
}

#[tokio::test]
async fn updating_an_integration_moves_it_last() {
    let workspace = workspace();
    let service = start(&workspace).await;
    service.integration("first", Some(integration("first", "one"))).await;
    service.integration("second", Some(integration("second", "two"))).await;
    assert_eq!(integration_ids(&service), ["first", "second"]);
    assert_eq!(shell(&service), Some("two".to_owned()));

    service.integration("first", Some(integration("first", "three"))).await;
    assert_eq!(integration_ids(&service), ["second", "first"]);
    assert_eq!(shell(&service), Some("three".to_owned()));
}

#[tokio::test]
async fn update_keeps_other_content_when_the_file_is_not_utf8() {
    let workspace = workspace();
    let file = workspace.config.join("opencode.jsonc");
    let mut bytes = b"// caf".to_vec();
    bytes.push(0xe9);
    bytes.extend_from_slice(b"\n{\"theme\": \"kept\"}\n");
    std::fs::write(&file, bytes).unwrap();
    let service = start(&workspace).await;

    service.update(&ConfigPatch { shell: Some("zsh".to_owned()) }).await.unwrap();

    let text = std::fs::read(&file).unwrap();
    let text = String::from_utf8_lossy(&text);
    assert!(text.contains("caf\u{fffd}"), "the lossily decoded comment is kept: {text}");
    assert!(text.contains("\"theme\""), "the other content is kept: {text}");
    assert!(text.contains("kept"), "the other content is kept: {text}");
    assert!(text.contains("zsh"), "the shell is written: {text}");
}

#[tokio::test]
async fn update_fails_when_the_config_file_cannot_be_read() {
    let workspace = workspace();
    std::fs::create_dir(workspace.config.join("opencode.jsonc")).unwrap();
    let service = start(&workspace).await;

    let result = service.update(&ConfigPatch { shell: Some("zsh".to_owned()) }).await;

    assert!(matches!(result, Err(ConfigUpdateError::Io(_))), "an unreadable file is an io error: {result:?}");
}

#[tokio::test]
async fn dropping_the_service_stops_its_background_tasks() {
    let workspace = workspace();
    let service = start(&workspace).await;
    let mut updated = service.subscribe_updated();

    drop(service);

    let closed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match updated.recv().await {
                Err(broadcast::error::RecvError::Closed) => return,
                Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => continue,
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "dropping the service ends its background tasks");
}

#[test]
fn plan_keys_include_the_watched_names() {
    let sources = |file: &str| Sources { direct: vec![PathBuf::from(file)], ..Sources::default() };
    let first = plan(&sources("/root/a.json"));
    let second = plan(&sources("/root/b.json"));
    assert_ne!(
        first.keys().collect::<Vec<_>>(),
        second.keys().collect::<Vec<_>>(),
        "a changed name set keys differently"
    );
    assert_eq!(
        first.keys().collect::<Vec<_>>(),
        plan(&sources("/root/a.json")).keys().collect::<Vec<_>>(),
        "the same target keys identically"
    );
}

#[test]
fn plan_matches_every_discover_corpus_case() {
    let cases = area("discover");
    assert_eq!(cases.len(), 21, "expected 21 discover cases, found {}", cases.len());
    for case in &cases {
        let sources = sources_from(&case.value["sources"]);
        let mut targets: Vec<Value> = plan(&sources).values().map(|target| json!(target)).collect();
        let mut expected: Vec<Value> = case.value["plan"].as_array().cloned().unwrap_or_default();
        sort_targets(&mut targets);
        sort_targets(&mut expected);
        assert_eq!(targets, expected, "{} plan", case.name);
    }
}

fn shell(service: &ConfigService) -> Option<String> {
    latest(&service.entries(), |info| info.shell.clone())
}

fn integration(id: &str, shell: &str) -> IntegrationDocument {
    IntegrationDocument {
        connection: json!({ "type": "credential", "id": id }),
        organization: None,
        info: parse_info(&format!(r#"{{"shell":"{shell}"}}"#), id).info.unwrap(),
    }
}

fn integration_ids(service: &ConfigService) -> Vec<String> {
    service
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            Entry::Document(document) => match &document.origin {
                Origin::Integration { integration_id, .. } => Some(integration_id.clone()),
                _ => None,
            },
            Entry::Directory { .. } => None,
        })
        .collect()
}

fn instructions(service: &ConfigService) -> Option<Vec<String>> {
    latest(&service.entries(), |info| info.instructions.clone())
}

async fn wait_updated(updated: &mut Receiver<()>) {
    tokio::time::timeout(Duration::from_secs(10), updated.recv())
        .await
        .expect("a config.updated event arrives")
        .expect("the config service is alive");
}

async fn assert_no_update(updated: &mut Receiver<()>) {
    assert!(
        tokio::time::timeout(Duration::from_millis(500), updated.recv()).await.is_err(),
        "no config.updated event arrives"
    );
}

async fn wait_change(changes: &mut Receiver<WatchUpdate>) -> WatchUpdate {
    tokio::time::timeout(Duration::from_secs(10), changes.recv())
        .await
        .expect("a change event arrives")
        .expect("the config service is alive")
}

fn sources_from(value: &Value) -> Sources {
    let path = |value: &Value| value.as_str().map(PathBuf::from);
    let list =
        |value: &Value| value.as_array().cloned().unwrap_or_default().iter().filter_map(path).collect::<Vec<PathBuf>>();
    Sources {
        system: value["system"].as_object().map(|system| Root {
            path: PathBuf::from(system["path"].as_str().unwrap()),
            present: system["present"].as_bool().unwrap(),
        }),
        preferences: value["preferences"].as_str().map(PathBuf::from),
        global: value["global"].as_str().map(PathBuf::from),
        explicit: value["explicit"].as_str().map(PathBuf::from),
        direct: list(&value["direct"]),
        project: value["project"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|root| Root {
                path: PathBuf::from(root["path"].as_str().unwrap()),
                present: root["present"].as_bool().unwrap(),
            })
            .collect(),
        claude: list(&value["claude"]),
        agents: list(&value["agents"]),
    }
}

fn sort_targets(targets: &mut [Value]) {
    targets.sort_by_key(target_key);
}

fn target_key(target: &Value) -> (String, String) {
    (target["path"].as_str().unwrap_or_default().to_owned(), target["type"].as_str().unwrap_or_default().to_owned())
}
