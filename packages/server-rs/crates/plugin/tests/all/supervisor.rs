use std::sync::{Arc, Mutex};
use std::time::Duration;

use opencode_config::parse::parse_info;
use opencode_config::service::{ConfigService, IntegrationDocument};
use opencode_plugin::generation::{Activator, PluginState, Source};
use opencode_plugin::host::{Entrypoints, LoadOutcome, ScriptHost};
use opencode_plugin::registry::Registry;
use opencode_plugin::sdk::{InstancePlugins, SdkPlugins};
use opencode_plugin::supervisor::Supervisor;
use opencode_plugin::update::INTERVAL;
use opencode_plugin::{GlobalPlugins, LocationPlugins, PluginRouter, Updates};
use serde_json::json;

use crate::support::{
    ConfigWorkspace, FakeHost, app, gated, loaded, local_source, location, log_entries, package_source, recorder,
    supervisor, wait_for,
};

fn log() -> Arc<Mutex<Vec<String>>> {
    Arc::new(Mutex::new(Vec::new()))
}

async fn start(
    workspace: &ConfigWorkspace,
    host: Option<Arc<FakeHost>>,
    sdk: SdkPlugins,
    instance: Arc<InstancePlugins>,
) -> (Supervisor, Registry, ConfigService) {
    let location = location(workspace.project.to_str().expect("the project path is UTF-8"));
    let registry = Registry::new(location.clone(), Arc::new(|| {}));
    let config = workspace.service().await;
    let host = host.map(|host| host as Arc<dyn ScriptHost>);
    let supervisor = supervisor(registry.clone(), config.clone(), host, location, sdk, instance);
    (supervisor, registry, config)
}

fn configured_plugin(workspace: &ConfigWorkspace, id: &str) -> (String, Arc<FakeHost>) {
    let directory = workspace.project.join("external").join(id);
    std::fs::create_dir_all(&directory).expect("create the configured plugin directory");
    let file = directory.join("index.ts");
    std::fs::write(&file, "export default {}").expect("write the configured plugin entry");
    let target = directory.to_string_lossy().into_owned();
    workspace.write_plugins(json!([target.clone()]));
    let host = FakeHost::new()
        .resolving(
            directory,
            Entrypoints {
                server: Some(url::Url::from_file_path(&file).expect("the entry path is absolute").to_string()),
                ..Default::default()
            },
        )
        .loading(target.clone(), loaded(id, "1", local_source(&target), "h1"));
    (target, Arc::new(host))
}

#[tokio::test(start_paused = true)]
async fn reports_a_duplicate_plugin_id_as_a_failure_without_dropping_the_generation() {
    let workspace = ConfigWorkspace::new();
    workspace.write_plugins(json!([]));
    let sdk = SdkPlugins::default();
    let log = log();
    sdk.register("account-prompts", recorder("account-prompts", "1", &log, false));
    let instance_activator: Arc<dyn Activator> = recorder("account-prompts", "instance", &log, false);
    let instance_extra: Arc<dyn Activator> = recorder("instance-extra", "instance", &log, false);
    let instance = Arc::new(
        InstancePlugins::bound(vec![
            ("account-prompts".to_string(), instance_activator),
            ("instance-extra".to_string(), instance_extra),
        ])
        .expect("the instance plugin IDs are unique"),
    );
    let (supervisor, registry, _config) = start(&workspace, None, sdk, instance).await;

    registry.await_activation().await;

    let inventory = registry.list();
    let duplicates = inventory.iter().filter(|info| info.id.as_deref() == Some("account-prompts")).collect::<Vec<_>>();
    assert_eq!(duplicates.len(), 2);
    assert_eq!(duplicates[0].source, Source::Sdk);
    assert_eq!(duplicates[0].state, PluginState::Active);
    assert_eq!(duplicates[1].source, Source::Sdk);
    assert_eq!(duplicates[1].features, opencode_plugin::generation::Features::server_only());
    assert_eq!(
        duplicates[1].state,
        PluginState::Failed { error: "Duplicate plugin ID: account-prompts".to_string(), reference: None }
    );
    assert!(inventory.iter().any(|info| {
        info.id.as_deref().is_some_and(|id| id.starts_with("opencode.")) && info.state == PluginState::Active
    }));
    assert_eq!(
        inventory
            .iter()
            .filter(|info| {
                info.id.as_deref().is_some_and(|id| id.starts_with("opencode.")) && info.state == PluginState::Active
            })
            .count(),
        opencode_plugin::generated::builtins::PRE.len() + opencode_plugin::generated::builtins::POST.len()
    );
    let ids = registry.active_ids();
    let last_pre = ids.iter().position(|id| id == "opencode.warming").expect("the last pre built-in is active");
    let sdk_index = ids.iter().position(|id| id == "account-prompts").expect("the SDK plugin is active");
    let instance_index = ids.iter().position(|id| id == "instance-extra").expect("the instance plugin is active");
    assert!(last_pre < sdk_index && sdk_index < instance_index);
    assert_eq!(
        log_entries(&log),
        vec!["activate:account-prompts:1".to_string(), "activate:instance-extra:instance".to_string()]
    );

    supervisor.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn activates_the_initial_generation_without_waiting_for_the_debounce() {
    let workspace = ConfigWorkspace::new();
    workspace.write_plugins(json!([]));
    let sdk = SdkPlugins::default();
    let log = log();
    sdk.register("initial", recorder("initial", "1", &log, false));
    let instance = Arc::new(InstancePlugins::empty());
    let (supervisor, registry, _config) = start(&workspace, None, sdk, instance).await;

    tokio::time::timeout(Duration::from_millis(50), registry.await_activation())
        .await
        .expect("the initial generation activates before the debounce elapses");

    assert!(registry.active_ids().iter().any(|id| id == "initial"));
    assert!(log_entries(&log).contains(&"activate:initial:1".to_string()));

    supervisor.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn reloads_for_a_trigger_published_while_the_initial_generation_is_activating() {
    let workspace = ConfigWorkspace::new();
    let (target, host) = configured_plugin(&workspace, "greeter");
    let sdk = SdkPlugins::default();
    let log = log();
    let gate = Arc::new(tokio::sync::Notify::new());
    let entered = Arc::new(tokio::sync::Notify::new());
    sdk.register("gated", gated("gated", "1", &log, gate.clone(), entered.clone()));
    let instance = Arc::new(InstancePlugins::empty());
    let (supervisor, registry, _config) = start(&workspace, Some(host.clone()), sdk.clone(), instance).await;

    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("the initial generation reaches the gated activation");
    sdk.register("late", recorder("late", "1", &log, false));
    gate.notify_one();
    registry.await_activation().await;
    wait_for(|| host.loads().len() >= 2).await;
    registry.await_activation().await;

    assert_eq!(host.loads(), vec![(target.clone(), false), (target.clone(), false)]);
    assert!(registry.active_ids().iter().any(|id| id == "late"));

    supervisor.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn keeps_policy_enforcement_and_the_console_connection_through_removals() {
    for targets in [
        &["opencode.config.policy", "opencode.provider.opencode", "opencode.config.agent"][..],
        &["*"][..],
        &["opencode.*"][..],
        &["opencode.config.policy"][..],
        &["opencode.provider.opencode"][..],
    ] {
        let workspace = ConfigWorkspace::new();
        workspace.write_plugins(json!(targets.iter().map(|target| format!("-{target}")).collect::<Vec<_>>()));
        let sdk = SdkPlugins::default();
        let log = log();
        sdk.register("custom", recorder("custom", "1", &log, false));
        let instance = Arc::new(InstancePlugins::empty());
        let (supervisor, registry, _config) = start(&workspace, None, sdk, instance).await;

        registry.await_activation().await;

        let inventory = registry.list();
        let status =
            |id: &str| inventory.iter().find(|info| info.id.as_deref() == Some(id)).map(|info| info.state.clone());
        assert_eq!(status("opencode.config.policy"), Some(PluginState::Active), "{targets:?}");
        assert_eq!(status("opencode.provider.opencode"), Some(PluginState::Active), "{targets:?}");
        match targets {
            ["*"] => {
                assert_eq!(status("custom"), None);
                assert_eq!(status("opencode.browser"), None);
                let mut ids = inventory.iter().filter_map(|info| info.id.clone()).collect::<Vec<_>>();
                ids.sort();
                assert_eq!(ids, vec!["opencode.config.policy".to_string(), "opencode.provider.opencode".to_string()]);
            }
            ["opencode.*"] => {
                assert_eq!(status("custom"), Some(PluginState::Active));
                assert_eq!(status("opencode.browser"), None);
            }
            [_] => {
                assert_eq!(
                    inventory.len(),
                    opencode_plugin::generated::builtins::PRE.len()
                        + opencode_plugin::generated::builtins::POST.len()
                        + 1
                );
                assert_eq!(status("custom"), Some(PluginState::Active));
            }
            _ => {
                assert_eq!(status("opencode.config.agent"), None);
                assert_eq!(status("custom"), Some(PluginState::Active));
                assert_eq!(
                    inventory.len(),
                    opencode_plugin::generated::builtins::PRE.len() + opencode_plugin::generated::builtins::POST.len()
                );
            }
        }

        supervisor.shutdown().await;
    }
}

#[tokio::test(start_paused = true)]
async fn coalesces_a_burst_of_reload_triggers_into_one_activation() {
    let workspace = ConfigWorkspace::new();
    let (_target, host) = configured_plugin(&workspace, "greeter");
    let sdk = SdkPlugins::default();
    let log = log();
    let instance = Arc::new(InstancePlugins::empty());
    let (supervisor, registry, _config) = start(&workspace, Some(host.clone()), sdk.clone(), instance).await;

    registry.await_activation().await;
    assert_eq!(host.loads().len(), 1);

    for id in ["a", "b", "c"] {
        sdk.register(id, recorder(id, "1", &log, false));
    }
    wait_for(|| host.loads().len() >= 2).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    registry.await_activation().await;

    assert_eq!(host.loads().len(), 2);
    for id in ["a", "b", "c"] {
        assert!(registry.active_ids().iter().any(|active| active == id));
    }
    let ids = registry.active_ids();
    let last_pre = ids.iter().position(|id| id == "opencode.warming").expect("the last pre built-in is active");
    let package = ids.iter().position(|id| id == "greeter").expect("the package is active");
    let first_post =
        ids.iter().position(|id| id == "opencode.config.instruction").expect("the first post built-in is active");
    assert!(last_pre < package && package < first_post);

    supervisor.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn reloads_all_booted_locations_when_sdk_register_is_called() {
    let first = ConfigWorkspace::new();
    first.write_plugins(json!([]));
    let second = ConfigWorkspace::new();
    second.write_plugins(json!([]));
    let sdk = SdkPlugins::default();
    let log = log();

    let (first_supervisor, first_registry, _first_config) =
        start(&first, None, sdk.clone(), Arc::new(InstancePlugins::empty())).await;
    let (second_supervisor, second_registry, _second_config) =
        start(&second, None, sdk.clone(), Arc::new(InstancePlugins::empty())).await;
    first_registry.await_activation().await;
    second_registry.await_activation().await;

    sdk.register("cross-location", recorder("cross-location", "1", &log, false));

    let active = |registry: &Registry| {
        registry
            .list()
            .iter()
            .any(|info| info.id.as_deref() == Some("cross-location") && info.state == PluginState::Active)
    };
    wait_for(|| active(&first_registry) && active(&second_registry)).await;
    assert!(active(&first_registry));
    assert!(active(&second_registry));

    first_supervisor.shutdown().await;
    second_supervisor.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn retains_a_discovered_and_a_configured_plugin_change_during_initial_activation() {
    for mode in ["discovered", "configured"] {
        let workspace = ConfigWorkspace::new();
        let (target, host) = if mode == "discovered" {
            let directory = workspace.plugins_dir();
            let file = directory.join("greeter.ts");
            std::fs::write(&file, "export default {}").expect("write the discovered plugin entry");
            workspace.write_plugins(json!([]));
            let target = file.to_string_lossy().into_owned();
            let host = FakeHost::new().loading(target.clone(), loaded("greeter", "1", local_source(&target), "h1"));
            (target, Arc::new(host))
        } else {
            let directory = workspace.project.join("external").join("greeter");
            std::fs::create_dir_all(&directory).expect("create the configured plugin directory");
            let file = directory.join("index.ts");
            std::fs::write(&file, "export default {}").expect("write the configured plugin entry");
            let target = directory.to_string_lossy().into_owned();
            workspace.write_plugins(json!([target.clone()]));
            let host = FakeHost::new()
                .resolving(
                    directory,
                    Entrypoints {
                        server: Some(url::Url::from_file_path(&file).expect("the entry path is absolute").to_string()),
                        ..Default::default()
                    },
                )
                .loading(target.clone(), loaded("greeter", "1", local_source(&target), "h1"));
            (target, Arc::new(host))
        };

        let sdk = SdkPlugins::default();
        let log = log();
        let gate = Arc::new(tokio::sync::Notify::new());
        let entered = Arc::new(tokio::sync::Notify::new());
        sdk.register("gated", gated("gated", "1", &log, gate.clone(), entered.clone()));
        let instance = Arc::new(InstancePlugins::empty());
        let (supervisor, registry, _config) = start(&workspace, Some(host.clone()), sdk.clone(), instance).await;

        tokio::time::timeout(Duration::from_secs(1), entered.notified())
            .await
            .expect("the initial activation reaches the gate");
        host.set_load(target.clone(), loaded("greeter", "2", local_source(&target), "h2"));
        sdk.register("late", recorder("late", "1", &log, false));
        gate.notify_one();
        registry.await_activation().await;
        wait_for(|| host.calls().iter().any(|call| call == "activate:h2")).await;
        registry.await_activation().await;

        assert!(host.calls().iter().any(|call| call == "activate:h2"), "{mode}");
        assert!(host.calls().iter().any(|call| call == "deactivate:activation:h1"), "{mode}");
        assert!(
            registry
                .list()
                .iter()
                .any(|info| { info.id.as_deref() == Some("greeter") && info.state == PluginState::Active })
        );

        supervisor.shutdown().await;
    }
}

#[tokio::test(start_paused = true)]
async fn enables_builtins_selected_by_add_operations() {
    let workspace = ConfigWorkspace::new();
    workspace.write_plugins(json!(["-*", "opencode.browser", "opencode.notreal"]));
    let sdk = SdkPlugins::default();
    let instance = Arc::new(InstancePlugins::empty());
    let (supervisor, registry, _config) = start(&workspace, None, sdk, instance).await;

    registry.await_activation().await;

    let inventory = registry.list();
    let mut ids = inventory.iter().filter_map(|info| info.id.clone()).collect::<Vec<_>>();
    ids.sort();
    assert_eq!(
        ids,
        vec![
            "opencode.browser".to_string(),
            "opencode.config.policy".to_string(),
            "opencode.provider.opencode".to_string(),
        ]
    );
    assert!(!ids.iter().any(|id| id == "opencode.notreal"));
    assert!(!ids.iter().any(|id| id == "opencode.tool.input.repair"));

    supervisor.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn keeps_the_running_package_when_its_new_revision_fails_to_load() {
    let workspace = ConfigWorkspace::new();
    workspace.write_plugins(json!(["pkg@1.0.0"]));
    let target = "pkg@1.0.0".to_string();
    let host =
        Arc::new(FakeHost::new().loading(target.clone(), loaded("pkg", "1", package_source(&target, "1.0.0"), "h1")));
    let sdk = SdkPlugins::default();
    let log = log();
    let instance = Arc::new(InstancePlugins::empty());
    let (supervisor, registry, _config) = start(&workspace, Some(host.clone()), sdk.clone(), instance).await;

    registry.await_activation().await;
    assert!(registry.list().iter().any(|info| info.id.as_deref() == Some("pkg") && info.state == PluginState::Active));

    host.set_load(target.clone(), LoadOutcome::Failed { message: "boom".to_owned() });
    sdk.register("trigger", recorder("trigger", "1", &log, false));
    wait_for(|| {
        registry.list().iter().any(|info| {
            matches!(info.state, PluginState::Failed { .. })
                && matches!(&info.source, Source::Package { target: listed, .. } if listed == &target)
        })
    })
    .await;

    assert!(registry.list().iter().any(|info| info.id.as_deref() == Some("pkg") && info.state == PluginState::Active));

    supervisor.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn drops_a_policy_blocked_package_and_its_failure() {
    let workspace = ConfigWorkspace::new();
    workspace.write_plugins(json!(["pkg@1.0.0"]));
    let target = "pkg@1.0.0".to_string();
    let host =
        Arc::new(FakeHost::new().loading(target.clone(), loaded("pkg", "1", package_source(&target, "1.0.0"), "h1")));
    let sdk = SdkPlugins::default();
    let log = log();
    let instance = Arc::new(InstancePlugins::empty());
    let (supervisor, registry, config) = start(&workspace, Some(host.clone()), sdk.clone(), instance).await;

    registry.await_activation().await;
    host.set_load(target.clone(), LoadOutcome::Failed { message: "boom".to_owned() });
    sdk.register("trigger", recorder("trigger", "1", &log, false));
    wait_for(|| {
        registry.list().iter().any(|info| {
            matches!(info.state, PluginState::Failed { .. })
                && matches!(&info.source, Source::Package { target: listed, .. } if listed == &target)
        })
    })
    .await;
    assert!(registry.list().iter().any(|info| info.id.as_deref() == Some("pkg") && info.state == PluginState::Active));

    let loads_before = host.loads().len();
    config
        .integration(
            "policy",
            Some(IntegrationDocument {
                connection: json!({"type": "credential", "id": "policy"}),
                organization: None,
                info: parse_info(
                    r#"{"policies":[{"action":"plugin.use","resource":"pkg","effect":"deny"}]}"#,
                    "policy",
                )
                .info
                .expect("the policy document parses"),
            }),
        )
        .await;
    wait_for(|| {
        registry.list().iter().all(|info| {
            let active = info.id.as_deref() == Some("pkg");
            let failed = matches!(&info.source, Source::Package { target: listed, .. } if listed == &target);
            !active && !failed
        })
    })
    .await;

    assert_eq!(host.loads().len(), loads_before);
    assert!(host.calls().iter().any(|call| call.starts_with("deactivate:")));

    supervisor.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn installs_only_after_activating_what_is_available_locally() {
    let workspace = ConfigWorkspace::new();
    let directory = workspace.project.join("external").join("installed");
    std::fs::create_dir_all(&directory).expect("create the configured plugin directory");
    let file = directory.join("index.ts");
    std::fs::write(&file, "export default {}").expect("write the configured plugin entry");
    let target = directory.to_string_lossy().into_owned();
    workspace.write_plugins(json!([target.clone()]));
    let host = Arc::new(
        FakeHost::new()
            .resolving(
                directory,
                Entrypoints {
                    server: Some(url::Url::from_file_path(&file).expect("the entry path is absolute").to_string()),
                    ..Default::default()
                },
            )
            .installing(target.clone(), loaded("installed", "1", local_source(&target), "h1")),
    );
    let sdk = SdkPlugins::default();
    let log = log();
    sdk.register("local", recorder("local", "1", &log, false));
    let instance = Arc::new(InstancePlugins::empty());
    let (supervisor, registry, _config) = start(&workspace, Some(host.clone()), sdk, instance).await;

    registry.await_activation().await;

    assert_eq!(host.loads(), vec![(target.clone(), false), (target.clone(), true)]);
    assert!(registry.active_ids().iter().any(|id| id == "local"));
    assert!(registry.active_ids().iter().any(|id| id == "installed"));
    assert!(host.calls().iter().any(|call| call == "activate:h1"));

    supervisor.shutdown().await;
}

fn package_decorations(registry: &Registry, target: &str) -> Option<(bool, bool)> {
    registry.list().iter().find_map(|info| match &info.source {
        Source::Package { target: listed, outdated, updating, .. } if listed == target => Some((*outdated, *updating)),
        _ => None,
    })
}

fn update_calls(host: &FakeHost, target: &str) -> usize {
    let key = format!("npm:update:{target}");
    host.calls().iter().filter(|call| **call == key).count()
}

fn check_calls(host: &FakeHost) -> usize {
    host.npm_calls("check")
}

#[tokio::test(start_paused = true)]
async fn refreshes_every_24_hours_without_an_immediate_reload() {
    let workspace = ConfigWorkspace::new();
    let target = "pkg@1.0.0";
    workspace.write_plugins(json!([target]));
    let host = Arc::new(FakeHost::new().loading(target, loaded("pkg", "1", package_source(target, "1.0.0"), "h1")));
    let (supervisor, registry, _config) =
        start(&workspace, Some(host.clone()), SdkPlugins::default(), Arc::new(InstancePlugins::empty())).await;

    registry.await_activation().await;
    assert_eq!(host.loads().len(), 1);

    tokio::time::advance(Duration::from_secs(23 * 60 * 60)).await;
    tokio::task::yield_now().await;
    assert_eq!(host.loads().len(), 1);

    tokio::time::advance(Duration::from_secs(60 * 60)).await;
    wait_for(|| host.loads().len() >= 2).await;
    registry.await_activation().await;

    tokio::time::advance(INTERVAL).await;
    wait_for(|| host.loads().len() >= 3).await;
    registry.await_activation().await;
    assert_eq!(host.loads().len(), 3);

    supervisor.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn updates_only_packages_that_follow_updates_once_per_check_interval() {
    let workspace = ConfigWorkspace::new();
    let follows = ["latest-plugin@latest", "ranged-plugin@^1.0.0", "github:acme/branch-plugin#dev"];
    let stays = ["plain-plugin", "next-plugin@next", "github:acme/default-plugin"];
    let targets = follows.iter().chain(stays.iter()).copied().collect::<Vec<_>>();
    workspace.write_plugins(json!(targets));

    let mut host = FakeHost::new();
    for (index, target) in targets.iter().enumerate() {
        host = host.loading(
            *target,
            loaded(&format!("plugin-{index}"), "1", package_source(target, "1.0.0"), &format!("h{index}")),
        );
        host.set_npm(&format!("check:{target}"), json!(true));
        host.set_npm(&format!("update:{target}"), json!({ "directory": "", "name": target }));
        host.set_npm(&format!("follows:{target}"), json!(follows.contains(target)));
    }
    let host = Arc::new(host);
    let (supervisor, registry, _config) =
        start(&workspace, Some(host.clone()), SdkPlugins::default(), Arc::new(InstancePlugins::empty())).await;

    registry.await_activation().await;
    wait_for(|| follows.iter().all(|target| update_calls(&host, target) >= 1)).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(stays.iter().all(|target| update_calls(&host, target) == 0));
    assert!(follows.iter().all(|target| update_calls(&host, target) == 1));

    tokio::time::advance(INTERVAL).await;
    wait_for(|| follows.iter().all(|target| update_calls(&host, target) >= 2)).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(stays.iter().all(|target| update_calls(&host, target) == 0));
    assert!(follows.iter().all(|target| update_calls(&host, target) == 2));

    supervisor.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn checks_and_updates_once_per_interval_24_hours_after_a_slow_startup() {
    let workspace = ConfigWorkspace::new();
    let target = "latest-plugin@latest";
    workspace.write_plugins(json!([target]));
    let host = Arc::new(FakeHost::new().loading(target, loaded("latest", "1", package_source(target, "1.0.0"), "h1")));
    host.set_npm(&format!("check:{target}"), json!(true));
    host.set_npm(&format!("follows:{target}"), json!(true));
    host.set_npm(&format!("update:{target}"), json!({ "directory": "", "name": target }));
    host.set_npm_delay(Duration::from_secs(10));
    let sdk = SdkPlugins::default();
    let log = log();
    let started = tokio::time::Instant::now();
    let (supervisor, registry, _config) =
        start(&workspace, Some(host.clone()), sdk.clone(), Arc::new(InstancePlugins::empty())).await;

    tokio::time::sleep(Duration::from_secs(25)).await;
    wait_for(|| update_calls(&host, target) >= 1).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    registry.await_activation().await;
    assert_eq!(update_calls(&host, target), 1);

    let checks = check_calls(&host);
    let loads = host.loads().len();
    sdk.register("interval-greet", recorder("interval-greet", "1", &log, false));
    wait_for(|| host.loads().len() > loads).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    registry.await_activation().await;
    assert_eq!(check_calls(&host), checks);
    assert_eq!(update_calls(&host, target), 1);

    host.set_npm_delay(Duration::ZERO);
    tokio::time::advance(started + INTERVAL - tokio::time::Instant::now()).await;
    wait_for(|| update_calls(&host, target) >= 2).await;
    assert!(check_calls(&host) > checks);

    supervisor.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn serializes_the_periodic_refresh_behind_an_in_flight_reload() {
    let workspace = ConfigWorkspace::new();
    let target = "pkg@1.0.0";
    workspace.write_plugins(json!([target]));
    let host = Arc::new(FakeHost::new().loading(target, loaded("pkg", "1", package_source(target, "1.0.0"), "h1")));
    let sdk = SdkPlugins::default();
    let log = log();
    let (supervisor, registry, _config) =
        start(&workspace, Some(host.clone()), sdk.clone(), Arc::new(InstancePlugins::empty())).await;

    registry.await_activation().await;
    let loads = host.loads().len();
    let gate = Arc::new(tokio::sync::Notify::new());
    host.set_load_gate(Some(gate.clone()));
    sdk.register("reload", recorder("reload", "1", &log, false));
    wait_for(|| host.load_inflight() == 1).await;
    assert_eq!(host.load_peak(), 1);

    tokio::time::advance(INTERVAL).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(host.load_peak(), 1);

    host.set_load_gate(None);
    gate.notify_one();
    wait_for(|| host.loads().len() > loads).await;
    registry.await_activation().await;

    assert_eq!(host.load_peak(), 1);

    supervisor.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn rejects_a_newer_version_that_fails_to_load() {
    let workspace = ConfigWorkspace::new();
    let target = "pkg@1.0.0";
    workspace.write_plugins(json!([target]));
    let host = Arc::new(FakeHost::new().loading(target, loaded("pkg", "1", package_source(target, "1.0.0"), "h1")));
    host.set_npm(&format!("resolve:{target}"), json!({ "directory": "", "name": "pkg", "version": "2.0.0" }));
    host.set_npm(&format!("reject:{target}@2.0.0"), json!(true));
    let sdk = SdkPlugins::default();
    let log = log();
    let (supervisor, registry, _config) =
        start(&workspace, Some(host.clone()), sdk.clone(), Arc::new(InstancePlugins::empty())).await;

    registry.await_activation().await;
    host.set_load(target, LoadOutcome::Failed { message: "boom".to_owned() });
    sdk.register("trigger", recorder("trigger", "1", &log, false));
    wait_for(|| host.npm_calls("reject") >= 1).await;

    assert_eq!(host.npm_calls("reject"), 1);
    assert!(registry.list().iter().any(|info| info.id.as_deref() == Some("pkg") && info.state == PluginState::Active));

    supervisor.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn marks_outdated_and_updating_sources_in_the_inventory() {
    let workspace = ConfigWorkspace::new();
    let target = "pkg@1.0.0";
    workspace.write_plugins(json!([target]));
    let host = Arc::new(FakeHost::new().loading(target, loaded("pkg", "1", package_source(target, "1.0.0"), "h1")));
    host.set_npm(&format!("check:{target}"), json!(true));
    let (supervisor, registry, _config) =
        start(&workspace, Some(host.clone()), SdkPlugins::default(), Arc::new(InstancePlugins::empty())).await;

    registry.await_activation().await;
    wait_for(|| package_decorations(&registry, target) == Some((true, false))).await;

    let gate = Arc::new(tokio::sync::Notify::new());
    host.set_npm_gate(Some(gate.clone()));
    let updates = supervisor.updates().clone();
    let update = tokio::spawn(async move { updates.update(target).await });
    wait_for(|| package_decorations(&registry, target) == Some((true, true))).await;

    host.set_npm_gate(None);
    gate.notify_one();
    update.await.expect("the update task ends").expect("the update succeeds");
    wait_for(|| package_decorations(&registry, target).is_some_and(|(_, updating)| !updating)).await;

    supervisor.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn a_superseded_location_does_not_close_the_host_location() {
    let workspace = ConfigWorkspace::new();
    let (_target, host) = configured_plugin(&workspace, "greeter");
    let location = location(workspace.project.to_str().expect("the project path is UTF-8"));
    let paths = workspace.npm_paths();
    let script_host: Arc<dyn ScriptHost> = host.clone();
    let global = GlobalPlugins {
        host: Some(script_host.clone()),
        router: PluginRouter::default(),
        sdk: SdkPlugins::default(),
        updates: Updates::new(Some(script_host), paths.clone()),
        paths,
        app: app(),
    };
    let config = workspace.service().await;
    let first = LocationPlugins::start(&global, location.clone(), config.clone(), Arc::new(|| {})).await;
    let second = LocationPlugins::start(&global, location, config, Arc::new(|| {})).await;
    first.await_activation().await;
    second.await_activation().await;

    first.shutdown().await;
    assert!(
        !host.calls().iter().any(|call| call == "close_location"),
        "a superseded location must not close the replacement's host state"
    );

    second.shutdown().await;
    assert!(host.calls().iter().any(|call| call == "close_location"));
}
