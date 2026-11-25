use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use opencode_plugin::generated::builtins::{GUARDED, POST, PRE};
use opencode_plugin::generation::{Features, Generation, Gui, Info, PluginState, Source};
use opencode_plugin::native;
use opencode_plugin::registry::{ActivateError, Registry};

use crate::support::{generation, location, log_entries, log_line, recorder};

fn registry() -> Registry {
    Registry::new(location("."), Arc::new(|| {}))
}

fn log() -> Arc<Mutex<Vec<String>>> {
    Arc::new(Mutex::new(Vec::new()))
}

#[tokio::test]
async fn keeps_the_unchanged_prefix_and_reactivates_the_suffix_in_order() {
    let log = log();
    let registry = registry();
    registry
        .activate(
            vec![
                generation("a", "1", recorder("a", "1", &log, false)),
                generation("b", "1", recorder("b", "1", &log, false)),
                generation("c", "1", recorder("c", "1", &log, false)),
            ],
            vec![],
        )
        .await
        .expect("the first activation succeeds");
    registry
        .activate(
            vec![
                generation("a", "1", recorder("a", "1", &log, false)),
                generation("b", "2", recorder("b", "2", &log, false)),
                generation("c", "2", recorder("c", "2", &log, false)),
            ],
            vec![],
        )
        .await
        .expect("the second activation succeeds");
    assert_eq!(
        log_line(&log),
        "activate:a:1, activate:b:1, activate:c:1, close:c:1, close:b:1, activate:b:2, activate:c:2"
    );
}

#[tokio::test]
async fn closes_the_suffix_in_reverse_order() {
    let log = log();
    let registry = registry();
    registry
        .activate(
            vec![
                generation("a", "1", recorder("a", "1", &log, false)),
                generation("b", "1", recorder("b", "1", &log, false)),
                generation("c", "1", recorder("c", "1", &log, false)),
            ],
            vec![],
        )
        .await
        .expect("the first activation succeeds");
    registry
        .activate(vec![generation("a", "1", recorder("a", "1", &log, false))], vec![])
        .await
        .expect("the shortened activation succeeds");
    assert_eq!(log_line(&log), "activate:a:1, activate:b:1, activate:c:1, close:c:1, close:b:1");
}

#[tokio::test]
async fn does_not_retry_a_failed_revision_until_it_changes() {
    let log = log();
    let registry = registry();
    registry
        .activate(vec![generation("a", "1", recorder("a", "1", &log, true))], vec![])
        .await
        .expect("a failed plugin still activates");
    registry
        .activate(vec![generation("a", "1", recorder("a", "1", &log, true))], vec![])
        .await
        .expect("the unchanged revision does not retry");
    assert_eq!(log_line(&log), "activate:a:1");
    registry
        .activate(vec![generation("a", "2", recorder("a", "2", &log, true))], vec![])
        .await
        .expect("the changed revision retries");
    assert_eq!(log_line(&log), "activate:a:1, activate:a:2");
}

#[tokio::test]
async fn keeps_the_previous_activation_when_a_new_revision_fails() {
    let log = log();
    let registry = registry();
    registry
        .activate(vec![generation("a", "1", recorder("a", "1", &log, false))], vec![])
        .await
        .expect("the first activation succeeds");
    registry
        .activate(vec![generation("a", "2", recorder("a", "2", &log, true))], vec![])
        .await
        .expect("a failing revision keeps the old activation");
    assert_eq!(log_line(&log), "activate:a:1, close:a:1, activate:a:2, activate:a:1");
    let listed = registry.list();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id.as_deref(), Some("a"));
    assert_eq!(listed[0].state, PluginState::Failed { error: "a@2 failed".to_string(), reference: None });
    registry.close().await;
    assert_eq!(log_line(&log), "activate:a:1, close:a:1, activate:a:2, activate:a:1, close:a:1");
}

#[tokio::test]
async fn appends_discovery_failures_after_active_slots() {
    let log = log();
    let registry = registry();
    let missing = Info {
        id: Some("missing".to_string()),
        source: Source::Builtin,
        features: Features::server_only(),
        state: PluginState::Failed { error: "missing failed".to_string(), reference: None },
    };
    registry
        .activate(vec![generation("a", "1", recorder("a", "1", &log, false))], vec![missing])
        .await
        .expect("activation succeeds");
    let listed = registry.list();
    assert_eq!(listed.iter().map(|info| info.id.as_deref()).collect::<Vec<_>>(), vec![Some("a"), Some("missing")]);
    assert!(matches!(listed[1].state, PluginState::Failed { .. }));
}

#[tokio::test]
async fn publishes_only_when_the_inventory_or_gui_revision_changes() {
    let log = log();
    let publishes = Arc::new(AtomicUsize::new(0));
    let counter = publishes.clone();
    let registry = Registry::new(
        location("."),
        Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }),
    );
    let plugin = |revision: &str| with_gui(generation("a", "1", recorder("a", "1", &log, false)), revision);
    registry.activate(vec![plugin("1")], vec![]).await.expect("the first activation publishes");
    assert_eq!(publishes.load(Ordering::SeqCst), 1);
    registry.activate(vec![plugin("1")], vec![]).await.expect("an identical activation stays quiet");
    assert_eq!(publishes.load(Ordering::SeqCst), 1);
    registry.activate(vec![plugin("2")], vec![]).await.expect("a changed gui revision publishes");
    assert_eq!(publishes.load(Ordering::SeqCst), 2);
    registry.activate(vec![plugin("2")], vec![]).await.expect("an identical activation stays quiet");
    assert_eq!(publishes.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn await_activation_waits_for_holds() {
    let registry = registry();
    let hold = registry.hold();
    let waiter = tokio::spawn({
        let registry = registry.clone();
        async move { registry.await_activation().await }
    });
    tokio::task::yield_now().await;
    assert!(!waiter.is_finished());
    hold.release();
    tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .expect("the hold release settles activation")
        .expect("the waiter joins");
}

#[tokio::test]
async fn disable_marks_the_plugin_failed_and_closes_it() {
    let log = log();
    let registry = registry();
    registry
        .activate(vec![generation("x", "1", recorder("x", "1", &log, false))], vec![])
        .await
        .expect("activation succeeds");
    let activation = registry.activation("x").expect("the plugin is active");
    registry.disable(&activation, "output");
    tokio::time::timeout(Duration::from_secs(1), registry.await_activation())
        .await
        .expect("the disabled plugin settles");
    let listed = registry.list();
    assert_eq!(listed.len(), 1);
    match &listed[0].state {
        PluginState::Failed { error, reference } => {
            assert_eq!(error, "Plugin disabled after output.transform failed. Check server logs for details.");
            assert!(reference.as_deref().is_some_and(|reference| reference.starts_with("err_")));
        }
        PluginState::Active => panic!("the disabled plugin should be failed"),
    }
    assert!(log_entries(&log).iter().any(|entry| entry == "close:x:1"));
}

#[tokio::test]
async fn close_tears_down_in_reverse_order() {
    let log = log();
    let registry = registry();
    registry
        .activate(
            vec![
                generation("a", "1", recorder("a", "1", &log, false)),
                generation("b", "1", recorder("b", "1", &log, false)),
                generation("c", "1", recorder("c", "1", &log, false)),
            ],
            vec![],
        )
        .await
        .expect("activation succeeds");
    registry.close().await;
    assert_eq!(log_line(&log), "activate:a:1, activate:b:1, activate:c:1, close:c:1, close:b:1, close:a:1");
    registry
        .activate(vec![generation("d", "1", recorder("d", "1", &log, false))], vec![])
        .await
        .expect("activation after close is a no-op");
    assert!(!log_line(&log).contains("activate:d:1"));
}

#[tokio::test]
async fn rejects_duplicate_ids() {
    let log = log();
    let registry = registry();
    let result = registry
        .activate(
            vec![
                generation("a", "1", recorder("a", "1", &log, false)),
                generation("a", "1", recorder("a", "1", &log, false)),
            ],
            vec![],
        )
        .await;
    assert!(matches!(result, Err(ActivateError::DuplicateId(id)) if id == "a"));
    assert!(log_entries(&log).is_empty());
}

#[test]
fn wire_info_matches_the_slot_shape() {
    let info = Info {
        id: Some("x".to_string()),
        source: Source::Package {
            target: "@scope/pkg".to_string(),
            version: Some("1.2.3".to_string()),
            outdated: true,
            updating: false,
        },
        features: Features { server: true, tui: true, rpc: false, gui: false },
        state: PluginState::Failed { error: "boom".to_string(), reference: Some("err_1234abcd".to_string()) },
    };
    let value: serde_json::Value =
        serde_json::from_str(&opencode_contract::codec::encode_json(&info.to_wire())).expect("the wire info is JSON");
    assert_eq!(
        value,
        serde_json::json!({
            "id": "x",
            "source": {"type": "package", "target": "@scope/pkg", "version": "1.2.3", "outdated": true},
            "features": {"server": true, "tui": true},
            "state": {"status": "failed", "error": "boom", "ref": "err_1234abcd"}
        })
    );
}

#[test]
fn builtin_catalog_matches_the_pin() {
    assert_eq!(PRE.len(), 80);
    assert_eq!(PRE[0], "opencode.tool.input.repair");
    assert_eq!(PRE[79], "opencode.warming");
    assert_eq!(POST.len(), 19);
    assert_eq!(POST[0], "opencode.config.instruction");
    assert_eq!(POST[18], "opencode.config.policy");
    assert_eq!(GUARDED, ["opencode.provider.opencode", "opencode.config.policy"].as_slice());

    let (pre, post) = native::builtins();
    assert_eq!(pre.len(), 80);
    assert_eq!(post.len(), 19);
    for plugin in pre.iter().chain(post.iter()) {
        assert_eq!(plugin.revision, "internal");
        assert_eq!(plugin.source, Some(Source::Builtin));
        assert!(plugin.features.is_none());
    }
    assert!(post.iter().any(|plugin| plugin.id == "opencode.browser"));
    assert!(post.iter().any(|plugin| plugin.id == "opencode.provider.dynamic"));
    assert!(native::guarded("opencode.provider.opencode"));
    assert!(native::guarded("opencode.config.policy"));
    assert!(!native::guarded("opencode.tool.read"));
}

fn with_gui(mut plugin: Generation, revision: &str) -> Generation {
    plugin.gui = Some(Gui {
        file: PathBuf::from("/tmp/gui-experimental.js"),
        revision: revision.to_string(),
        options: serde_json::Map::new(),
    });
    plugin
}
