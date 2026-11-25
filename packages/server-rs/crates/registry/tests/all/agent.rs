use std::sync::{Arc, Mutex};

use opencode_contract::generated::{AgentInfoMode, PermissionEffect, PermissionRule};
use opencode_registry::agent::{AgentService, default_info};
use opencode_registry::builtin;
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

fn updates(recorder: &Recorder) -> Vec<&'static str> {
    recorder.events.lock().expect("the event lock is not poisoned").clone()
}

#[tokio::test]
async fn publishes_one_update_event_for_a_change() {
    let recorder = Arc::new(Recorder::default());
    let service = AgentService::new(recorder.clone());
    let _first = service.transform(|editor| editor.update("reviewer", |_| {})).await;
    assert_eq!(updates(&recorder), ["agent.updated"]);
    let _second = service.transform(|_| {}).await;
    assert_eq!(updates(&recorder), ["agent.updated"]);
}

#[tokio::test]
async fn starts_empty() {
    let service = AgentService::new(Arc::new(Recorder::default()));
    assert!(service.list().await.expect("the state is readable").is_empty());
    assert!(service.get("build").await.expect("the state is readable").is_none());
}

#[tokio::test]
async fn replays_transforms() {
    let service = AgentService::new(Arc::new(Recorder::default()));
    let _registration = service
        .transform(|editor| {
            editor.update("reviewer", |info| {
                info.description = Some("Reviews code".to_string());
                info.mode = AgentInfoMode::Subagent;
            });
        })
        .await;

    let info = service.get("reviewer").await.expect("the state is readable").expect("the agent exists");
    assert_eq!(info.id.0, "reviewer");
    assert_eq!(info.description.as_deref(), Some("Reviews code"));
    assert_eq!(info.mode, AgentInfoMode::Subagent);
    let ids: Vec<String> =
        service.list().await.expect("the state is readable").into_iter().map(|info| info.id.0).collect();
    assert_eq!(ids, ["reviewer"]);
}

#[tokio::test]
async fn removes_a_disposed_transform() {
    let service = AgentService::new(Arc::new(Recorder::default()));
    let registration = service.transform(|editor| editor.update("scoped", |_| {})).await;
    assert!(service.get("scoped").await.expect("the state is readable").is_some());
    registration.dispose().await;
    assert!(service.get("scoped").await.expect("the state is readable").is_none());
}

#[tokio::test]
async fn lists_the_selected_default_first() {
    let service = AgentService::new(Arc::new(Recorder::default()));
    let _registration = service
        .transform(|editor| {
            editor.update("build", |info| info.mode = AgentInfoMode::Primary);
            editor.update("reviewer", |info| info.mode = AgentInfoMode::Primary);
            editor.update("explore", |info| info.mode = AgentInfoMode::Subagent);
            editor.default(Some("reviewer"));
        })
        .await;

    let ids: Vec<String> =
        service.list().await.expect("the state is readable").into_iter().map(|info| info.id.0).collect();
    assert_eq!(ids, ["reviewer", "build", "explore"]);
}

#[tokio::test]
async fn creates_runtime_defaults_and_removes_them() {
    let service = AgentService::new(Arc::new(Recorder::default()));
    let _registration = service.transform(|editor| editor.update("custom", |_| {})).await;

    let info = service.get("custom").await.expect("the state is readable").expect("the agent exists");
    assert_eq!(info.mode, AgentInfoMode::Primary);
    assert!(!info.hidden);
    assert_eq!(info.permissions, default_info("custom").permissions);

    let _removal = service.transform(|editor| editor.remove("custom")).await;
    assert!(service.get("custom").await.expect("the state is readable").is_none());
}

#[tokio::test]
async fn applies_builtin_agents() {
    let home = TempDir::new();
    let path = home.path().to_path_buf();
    let service = AgentService::new(Arc::new(Recorder::default()));
    let _builtins = service.transform(builtin::agent::agent).await;
    let _plan = service
        .transform({
            let path = path.clone();
            move |editor| builtin::agent::plan(editor, &path)
        })
        .await;

    let agents = service.list().await.expect("the state is readable");
    let ids: Vec<&str> = agents.iter().map(|info| info.id.0.as_str()).collect();
    assert_eq!(ids, ["build", "general", "explore", "compaction", "title", "summary", "plan"]);

    let explore = agents.iter().find(|info| info.id.0 == "explore").expect("explore is listed");
    assert_eq!(explore.system.as_deref(), Some(builtin::agent::PROMPT_EXPLORE));

    let plan = agents.iter().find(|info| info.id.0 == "plan").expect("plan is listed");
    let directory = path.join(".opencode").join("plan").join("*").to_string_lossy().to_string();
    let mut expected = default_info("plan").permissions;
    expected.extend([
        PermissionRule { action: "question".to_string(), resource: "*".to_string(), effect: PermissionEffect::Allow },
        PermissionRule { action: "edit".to_string(), resource: "*".to_string(), effect: PermissionEffect::Deny },
        PermissionRule { action: "edit".to_string(), resource: directory, effect: PermissionEffect::Allow },
    ]);
    assert_eq!(plan.permissions, expected);
}
