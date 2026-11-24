use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use opencode_config::entry::{Entry, latest};
use opencode_config::observer;
use opencode_config::options::{ConfigLocation, ConfigOptions, SystemOption};
use opencode_config::paths::GlobalPaths;
use opencode_config::seams::NoKv;
use opencode_config::service::{ConfigService, ServiceDeps};
use opencode_config::wellknown::WellKnown;
use opencode_util::temp::TempDir;
use tokio::sync::mpsc;

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
async fn observe_closes_the_startup_race_and_reloads_later_updates() {
    let workspace = workspace();
    std::fs::write(workspace.project.join("opencode.json"), r#"{"shell":"first"}"#).unwrap();
    let service = start(&workspace).await;
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let handle = observer::observe(&service, move |entries| {
        sender.send(latest(entries, |info| info.shell.clone())).expect("the observer channel is open");
    });

    assert_eq!(latest(&handle.entries(), |info| info.shell.clone()), Some("first".to_owned()));
    assert!(
        tokio::time::timeout(Duration::from_millis(200), receiver.recv()).await.is_err(),
        "the initial snapshot does not reload"
    );

    std::fs::write(workspace.project.join("opencode.json"), r#"{"shell":"later"}"#).unwrap();
    let observed = tokio::time::timeout(Duration::from_secs(10), receiver.recv())
        .await
        .expect("a reload notification arrives")
        .expect("the observer is alive");
    assert_eq!(observed, Some("later".to_owned()));
    assert_eq!(latest(&handle.entries(), |info| info.shell.clone()), Some("later".to_owned()));
}

#[tokio::test]
async fn observe_sources_applies_the_initial_load_and_fires_for_a_directory_change() {
    let workspace = workspace();
    std::fs::create_dir_all(workspace.project.join(".opencode/agent")).unwrap();
    let service = start(&workspace).await;
    let expected = vec![workspace.project.join(".opencode").to_string_lossy().into_owned()];
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let _handle = observer::observe_sources(&service, &["agent"], directory_paths, move |paths| {
        sender.send(paths).expect("the observer channel is open");
    });

    assert_eq!(receiver.try_recv().expect("the initial load is applied"), expected);

    std::fs::write(workspace.project.join(".opencode/agent/probe.md"), "probe").unwrap();
    let updated = tokio::time::timeout(Duration::from_secs(10), receiver.recv())
        .await
        .expect("a reload notification arrives")
        .expect("the observer is alive");
    assert_eq!(updated, expected);
}

#[tokio::test]
async fn observe_sources_ignores_an_unrelated_file() {
    let workspace = workspace();
    std::fs::create_dir_all(workspace.project.join(".opencode/agent")).unwrap();
    let service = start(&workspace).await;
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let _handle = observer::observe_sources(&service, &["agent"], directory_paths, move |paths| {
        sender.send(paths).expect("the observer channel is open");
    });

    receiver.try_recv().expect("the initial load is applied");

    std::fs::write(workspace.project.join("notes.txt"), "notes").unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(500), receiver.recv()).await.is_err(),
        "an unrelated file does not reload"
    );
}

#[tokio::test]
async fn dropping_the_handle_stops_reloads() {
    let workspace = workspace();
    std::fs::write(workspace.project.join("opencode.json"), r#"{"shell":"first"}"#).unwrap();
    let service = start(&workspace).await;
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let handle = observer::observe(&service, move |entries| {
        sender.send(latest(entries, |info| info.shell.clone())).expect("the observer channel is open");
    });

    std::fs::write(workspace.project.join("opencode.json"), r#"{"shell":"later"}"#).unwrap();
    let observed = tokio::time::timeout(Duration::from_secs(10), receiver.recv())
        .await
        .expect("a reload notification arrives")
        .expect("the observer is alive");
    assert_eq!(observed, Some("later".to_owned()));

    drop(handle);

    std::fs::write(workspace.project.join("opencode.json"), r#"{"shell":"third"}"#).unwrap();
    let quiet = tokio::time::timeout(Duration::from_millis(500), receiver.recv()).await;
    assert!(!matches!(quiet, Ok(Some(_))), "dropping the handle stops reloads");
}

fn directory_paths(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Directory { path } => Some(path.to_string_lossy().into_owned()),
            Entry::Document(_) => None,
        })
        .collect()
}
