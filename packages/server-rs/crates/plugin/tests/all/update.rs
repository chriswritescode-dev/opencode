use std::sync::Arc;
use std::time::Duration;

use opencode_plugin::host::{HostError, ScriptHost};
use opencode_plugin::update::{Change, Updates};
use serde_json::json;

use crate::support::{FakeHost, npm_paths};

const TARGET: &str = "pkg@1.0.0";

fn updates(host: &Arc<FakeHost>) -> Updates {
    Updates::new(Some(host.clone() as Arc<dyn ScriptHost>), npm_paths())
}

fn error() -> HostError {
    HostError { tag: "Test".to_string(), message: "boom".to_string() }
}

fn change(outdated: bool, updating: bool) -> Change {
    Change { target: TARGET.to_string(), outdated, updating }
}

#[tokio::test(start_paused = true)]
async fn check_reuses_the_status_within_a_cycle() {
    let host = Arc::new(FakeHost::new());
    host.set_npm(&format!("check:{TARGET}"), json!(true));
    let updates = updates(&host);

    assert!(updates.check(TARGET, false).await);
    assert!(updates.check(TARGET, false).await);

    assert_eq!(host.npm_calls("check"), 1);
}

#[tokio::test(start_paused = true)]
async fn refresh_asks_again() {
    let host = Arc::new(FakeHost::new());
    host.set_npm(&format!("check:{TARGET}"), json!(true));
    let updates = updates(&host);

    assert!(updates.check(TARGET, false).await);
    assert!(updates.check(TARGET, true).await);

    assert_eq!(host.npm_calls("check"), 2);
}

#[tokio::test(start_paused = true)]
async fn a_failed_check_keeps_the_previous_value() {
    let host = Arc::new(FakeHost::new());
    host.set_npm(&format!("check:{TARGET}"), json!(true));
    let updates = updates(&host);
    assert!(updates.check(TARGET, false).await);

    host.set_npm_error(&format!("check:{TARGET}"), error());
    assert!(updates.check(TARGET, true).await);
    assert_eq!(host.npm_calls("check"), 2);

    host.clear_npm_error(&format!("check:{TARGET}"));
    host.set_npm(&format!("check:{TARGET}"), json!(false));
    assert!(!updates.check(TARGET, true).await);
}

#[tokio::test(start_paused = true)]
async fn update_reports_updating_then_clears_outdated() {
    let host = Arc::new(FakeHost::new());
    host.set_npm(&format!("check:{TARGET}"), json!(true));
    let updates = updates(&host);
    let mut changes = updates.changes();
    assert!(updates.check(TARGET, false).await);
    assert_eq!(changes.recv().await.expect("the check reports the change"), change(true, false));

    updates.update(TARGET).await.expect("the update succeeds");
    assert_eq!(changes.recv().await.expect("the update starts"), change(true, true));
    assert_eq!(changes.recv().await.expect("the update finishes"), change(false, false));

    assert!(updates.check(TARGET, false).await);
    assert_eq!(host.npm_calls("check"), 2);
}

#[tokio::test(start_paused = true)]
async fn a_failed_update_keeps_outdated() {
    let host = Arc::new(FakeHost::new());
    host.set_npm(&format!("check:{TARGET}"), json!(true));
    host.set_npm_error(&format!("update:{TARGET}"), error());
    let updates = updates(&host);
    let mut changes = updates.changes();
    assert!(updates.check(TARGET, false).await);
    assert_eq!(changes.recv().await.expect("the check reports the change"), change(true, false));

    assert!(updates.update(TARGET).await.is_err());
    assert_eq!(changes.recv().await.expect("the update starts"), change(true, true));
    assert_eq!(changes.recv().await.expect("the update fails"), change(true, false));

    assert!(updates.check(TARGET, false).await);
    assert_eq!(host.npm_calls("check"), 1);
}

#[tokio::test(start_paused = true)]
async fn updates_for_one_target_are_serialized() {
    let host = Arc::new(FakeHost::new());
    let gate = Arc::new(tokio::sync::Notify::new());
    host.set_npm_gate(Some(gate.clone()));
    let updates = updates(&host);
    let mut changes = updates.changes();

    let first = {
        let updates = updates.clone();
        tokio::spawn(async move { updates.update(TARGET).await })
    };
    assert_eq!(
        tokio::time::timeout(Duration::from_millis(100), changes.recv())
            .await
            .expect("the first update starts")
            .expect("the change channel is open"),
        change(false, true)
    );
    let second = {
        let updates = updates.clone();
        tokio::spawn(async move { updates.update(TARGET).await })
    };
    tokio::time::sleep(Duration::from_millis(10)).await;

    assert_eq!(host.npm_inflight(), 1);
    assert_eq!(host.npm_peak(), 1);
    assert_eq!(host.npm_calls("update"), 0);

    host.set_npm_gate(None);
    gate.notify_one();
    first.await.expect("the first update task ends").expect("the first update succeeds");
    second.await.expect("the second update task ends").expect("the second update succeeds");

    assert_eq!(host.npm_peak(), 1);
    assert_eq!(host.npm_calls("update"), 2);
}
