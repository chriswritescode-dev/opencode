use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use opencode_config::parse::parse_info;
use opencode_config::service::{ConfigService, IntegrationDocument};
use opencode_plugin::generation::{PluginState, Source};
use opencode_plugin::host::{ScriptHost, location_ref};
use opencode_plugin::registry::Registry;
use opencode_plugin::router::{PluginRouter, RouterGuard};
use opencode_plugin::sdk::{InstancePlugins, SdkPlugins};
use opencode_plugin::sidecar::SidecarHost;
use opencode_plugin::supervisor::{Supervisor, SupervisorDeps, Updates};
use opencode_plugin::watch::{ManualWatcher, PluginWatcher};
use opencode_sidecar::config::Config;
use opencode_sidecar::connection::{Handlers, Request};
use opencode_sidecar::supervisor::Sidecar;
use serde_json::json;

use crate::support::{ConfigWorkspace, app, location};

struct Harness {
    workspace: ConfigWorkspace,
    watcher: Arc<ManualWatcher>,
    registry: Registry,
    config: ConfigService,
    sidecar: Sidecar,
    supervisor: Supervisor,
    _guard: RouterGuard,
}

impl Harness {
    async fn start(workspace: ConfigWorkspace) -> Option<Harness> {
        Harness::start_with(workspace, Duration::ZERO).await
    }

    async fn start_with(workspace: ConfigWorkspace, backoff: Duration) -> Option<Harness> {
        let location = location(workspace.project.to_str().expect("the project path is UTF-8"));
        let registry = Registry::new(location.clone(), Arc::new(|| {}));
        let config = workspace.service().await;
        let watcher = Arc::new(ManualWatcher::new());
        let watcher_trait: Arc<dyn PluginWatcher> = watcher.clone();
        let router = PluginRouter::default();
        let mut handlers = Handlers::new();
        router.handlers(&mut handlers);
        let mut sidecar_config = sidecar_config(&workspace, backoff);
        let Some(runtime) = sidecar_config.runtime().await else {
            eprintln!("skipping: neither Bun nor an opencode runtime was found");
            return None;
        };
        sidecar_config.bun = Some(runtime);
        let sidecar = Sidecar::new(sidecar_config, handlers);
        let guard = router.register(location_ref(&location), watcher_trait.clone(), registry.clone());
        let host: Arc<dyn ScriptHost> = Arc::new(SidecarHost::new(sidecar.clone()));
        let paths = workspace.npm_paths();
        let supervisor = Supervisor::start(SupervisorDeps {
            registry: registry.clone(),
            config: config.clone(),
            host: Some(host.clone()),
            paths: paths.clone(),
            sdk: SdkPlugins::default(),
            instance: Arc::new(InstancePlugins::empty()),
            updates: Updates::new(Some(host), paths),
            location,
            app: app(),
            watcher: Some(watcher_trait),
            modules: watcher.changes(),
        });
        Some(Harness { workspace, watcher, registry, config, sidecar, supervisor, _guard: guard })
    }

    async fn shutdown(&self) {
        self.supervisor.shutdown().await;
        self.sidecar.shutdown().await;
    }
}

fn sidecar_config(workspace: &ConfigWorkspace, backoff: Duration) -> Config {
    let vars = ["PATH"]
        .into_iter()
        .filter_map(|name| std::env::var_os(name).map(|value| (OsString::from(name), value)))
        .chain(std::iter::once((OsString::from("HOME"), workspace.home.clone().into_os_string())))
        .collect::<Vec<_>>();
    let mut config = Config::new(vars, workspace.project.clone(), std::env::temp_dir());
    config.test_methods = true;
    config.limits.backoff_initial = backoff;
    config.limits.backoff_max = backoff;
    config.limits.handshake = Duration::from_secs(30);
    config
}

fn js(value: impl AsRef<str>) -> String {
    serde_json::Value::String(value.as_ref().to_owned()).to_string()
}

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create the fixture directory");
    }
    std::fs::write(path, text).expect("write the fixture");
}

fn config(workspace: &ConfigWorkspace, value: serde_json::Value) {
    workspace.write_config(&value.to_string());
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn effect_plugin(id: &str, setup: &Path, close: Option<&Path>) -> String {
    let finalizer = close
        .map(|path| {
            format!(
                "    yield* Effect.addFinalizer(() => Effect.sync(() => appendFileSync({}, {})))\n",
                js(path.to_string_lossy()),
                js(format!("closed:{id}\n"))
            )
        })
        .unwrap_or_default();
    format!(
        "import {{ Effect }} from \"effect\"\nimport {{ appendFileSync }} from \"node:fs\"\nexport default {{\n  id: {},\n  effect: () => Effect.gen(function* () {{\n    appendFileSync({}, {})\n{}  }}),\n}}\n",
        js(id),
        js(setup.to_string_lossy()),
        js(format!("setup:{id}\n")),
        finalizer
    )
}

fn greeter_helper(version: u32, setup: &Path, close: &Path) -> String {
    format!(
        "import {{ Effect }} from \"effect\"\nimport {{ appendFileSync }} from \"node:fs\"\nexport default {{\n  id: \"greeter\",\n  effect: () => Effect.gen(function* () {{\n    appendFileSync({}, {})\n    yield* Effect.addFinalizer(() => Effect.sync(() => appendFileSync({}, {})))\n  }}),\n}}\n",
        js(setup.to_string_lossy()),
        js(format!("v{version}\n")),
        js(close.to_string_lossy()),
        js(format!("closed:v{version}\n"))
    )
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn active_ids(registry: &Registry) -> Vec<String> {
    registry.active_ids()
}

async fn wait_until(mut predicate: impl FnMut() -> bool, message: &str) {
    for _ in 0..2000 {
        if predicate() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{message}");
}

#[tokio::test]
async fn loads_a_discovered_local_plugin_and_tears_it_down_on_close() {
    let workspace = ConfigWorkspace::new();
    let plugins = workspace.plugins_dir();
    let setup = workspace.project.join("setup.log");
    let close = workspace.project.join("close.log");
    for id in ["a", "b"] {
        write(&plugins.join(id).join("index.ts"), &effect_plugin(id, &setup, Some(&close)));
    }

    let Some(harness) = Harness::start(workspace).await else { return };
    harness.registry.await_activation().await;
    wait_until(|| read(&setup).lines().count() == 2, "both plugins run their setup").await;
    let ids = active_ids(&harness.registry);
    assert!(ids.contains(&"a".to_string()) && ids.contains(&"b".to_string()), "{ids:?}");

    harness.shutdown().await;

    let closes = read(&close);
    assert_eq!(closes.lines().collect::<Vec<_>>(), vec!["closed:b", "closed:a"]);
}

#[tokio::test]
async fn blocks_a_local_plugin_before_module_initialization_and_honors_replacement_policies() {
    let workspace = ConfigWorkspace::new();
    let root = workspace.project.join("external").join("policy-fixture");
    let entered = workspace.project.join("entered");
    write(
        &root.join("index.ts"),
        &format!(
            "await Bun.write({}, \"loaded\")\nexport default {{ id: \"policy-fixture\", async setup() {{}} }}\n",
            js(path_string(&entered))
        ),
    );
    let target = path_string(&root);
    config(
        &workspace,
        json!({
            "plugins": [target],
            "policies": [{ "action": "plugin.use", "resource": "*", "effect": "deny" }],
        }),
    );

    let Some(harness) = Harness::start(workspace).await else { return };
    harness.registry.await_activation().await;
    assert!(!entered.exists(), "a denied plugin must not be imported");
    assert!(!active_ids(&harness.registry).contains(&"policy-fixture".to_string()));

    config(&harness.workspace, json!({ "plugins": [target] }));
    harness.config.reload().await;
    harness.watcher.emit();
    wait_until(|| entered.exists(), "an allowed plugin is imported").await;
    wait_until(
        || {
            harness
                .registry
                .list()
                .iter()
                .any(|info| info.id.as_deref() == Some("policy-fixture") && info.state == PluginState::Active)
        },
        "the allowed plugin is active",
    )
    .await;

    harness.shutdown().await;
}

#[tokio::test]
async fn blocks_a_versioned_package_before_npm_resolution() {
    let workspace = ConfigWorkspace::new();
    config(
        &workspace,
        json!({
            "plugins": ["policy-fixture@1.0.0", "policy-fixture@^1", "@scope/policy-fixture@latest"],
            "policies": [
                { "action": "plugin.use", "resource": "policy-fixture", "effect": "deny" },
                { "action": "plugin.use", "resource": "@scope/policy-fixture", "effect": "deny" },
            ],
        }),
    );
    let cache = workspace.paths().cache.clone();

    let Some(harness) = Harness::start(workspace).await else { return };
    harness.registry.await_activation().await;

    assert!(!cache.join("npm").exists(), "a denied package must not be resolved or installed");
    assert!(harness.registry.list().iter().all(|info| !matches!(&info.source, Source::Package { target, .. } if target.starts_with("policy-fixture") || target.starts_with("@scope/policy-fixture"))));

    harness.shutdown().await;
}

#[tokio::test]
async fn loads_a_gui_only_package_under_its_package_name_without_running_it() {
    let workspace = ConfigWorkspace::new();
    let gui = workspace.project.join("gui");
    let bundle = gui.join("dist").join("gui-experimental.cjs");
    write(
        &gui.join("package.json"),
        "{\"name\":\"opencode-acme-gui\",\"exports\":{\"./gui-experimental\":\"./dist/gui-experimental.cjs\"}}",
    );
    write(&bundle, "throw new Error(\"A client part must not run on the server\")");
    let tui = workspace.project.join("acme-tui");
    write(&tui.join("tui.tsx"), "throw new Error(\"A client part must not run on the server\")");

    config(
        &workspace,
        json!({
            "plugins": [
                { "package": path_string(&gui), "options": { "region": "eu" } },
                path_string(&tui),
            ],
        }),
    );

    let Some(harness) = Harness::start(workspace).await else { return };
    harness.registry.await_activation().await;

    let listed = harness.registry.list();
    let entry =
        listed.iter().find(|info| info.id.as_deref() == Some("opencode-acme-gui")).expect("the GUI plugin is listed");
    assert_eq!(entry.source, Source::Local { path: path_string(&gui) });
    assert!(entry.features.gui && !entry.features.server);
    assert_eq!(entry.state, PluginState::Active);
    let value = harness.registry.gui("opencode-acme-gui").expect("the GUI bundle");
    assert_eq!(value.file, bundle);
    assert_eq!(value.options.get("region"), Some(&json!("eu")));
    assert!(
        listed.iter().all(|info| !matches!(&info.source, Source::Local { path } if path == &path_string(&tui))),
        "a TUI-only directory stays the TUI's own"
    );

    harness.shutdown().await;
}

async fn helper_reload(helper: &str, touch_entry: bool) {
    let workspace = ConfigWorkspace::new();
    let root = workspace.project.join("external").join("greeter");
    let entry = root.join("index.ts");
    let helper_file = root.join(helper);
    let setup = workspace.project.join("setup.log");
    let close = workspace.project.join("close.log");
    write(&entry, &format!("export {{ default }} from {}\n", js(helper)));
    write(&helper_file, &greeter_helper(1, &setup, &close));
    config(&workspace, json!({ "plugins": [path_string(&root)] }));

    let Some(harness) = Harness::start(workspace).await else { return };
    harness.registry.await_activation().await;
    wait_until(|| read(&setup).contains("v1"), "the first helper version runs").await;

    write(&helper_file, &greeter_helper(2, &setup, &close));
    if touch_entry {
        write(&entry, &format!("export {{ default }} from {}\n// updated entry\n", js(helper)));
    }
    harness.watcher.emit();
    wait_until(|| read(&setup).contains("v2"), "the helper change reloads the plugin").await;

    write(&helper_file, "throw new Error(\"broken helper\"); export default {}");
    harness.watcher.emit();
    wait_until(
        || {
            harness.registry.list().iter().any(|info| {
                matches!(info.state, PluginState::Failed { .. })
                    && matches!(&info.source, Source::Local { path } if path == &path_string(&root))
            })
        },
        "the broken helper is reported failed",
    )
    .await;
    assert!(active_ids(&harness.registry).contains(&"greeter".to_string()));
    assert!(!read(&setup).contains("v3"), "the failed revision does not run its setup");
    assert!(!read(&close).contains("closed:v2"), "the failed revision keeps the running activation");

    write(&helper_file, &greeter_helper(3, &setup, &close));
    harness.watcher.emit();
    wait_until(|| read(&setup).contains("v3"), "the fixed helper reloads").await;
    wait_until(|| read(&close).contains("closed:v2"), "the replaced activation is torn down").await;

    harness.shutdown().await;
}

#[tokio::test]
async fn reloads_helper_defined_behaviour_on_a_helper_only_save() {
    helper_reload("./nested/helper.ts", false).await;
}

#[tokio::test]
async fn reloads_helper_defined_behaviour_when_the_entrypoint_also_changes() {
    helper_reload("./nested/helper.ts", true).await;
}

#[tokio::test]
async fn reloads_helper_defined_behaviour_outside_the_configured_directory() {
    helper_reload("../shared/helper.ts", false).await;
}

#[tokio::test]
async fn watches_creation_of_an_external_helper_with_missing_parent_directories() {
    let workspace = ConfigWorkspace::new();
    let root = workspace.project.join("plugin");
    let shared = workspace.project.join("shared");
    std::fs::create_dir_all(&shared).expect("create the existing ancestor");
    write(&root.join("index.ts"), "export { default } from \"../shared/new/nested/helper.ts\"\n");
    config(&workspace, json!({ "plugins": [path_string(&root)] }));

    let Some(harness) = Harness::start(workspace).await else { return };
    harness.registry.await_activation().await;
    assert!(
        harness.watcher.watched().iter().any(|path| path == &shared),
        "the missing helper directory falls back to its nearest existing ancestor: {:?}",
        harness.watcher.watched()
    );

    write(
        &shared.join("new").join("nested").join("helper.ts"),
        "export default { id: \"appeared\", async setup() {} }\n",
    );
    harness.watcher.emit();
    wait_until(
        || {
            harness
                .registry
                .list()
                .iter()
                .any(|info| info.id.as_deref() == Some("appeared") && info.state == PluginState::Active)
        },
        "the created helper loads",
    )
    .await;

    harness.shutdown().await;
}

#[tokio::test]
async fn keeps_the_running_generation_when_an_updated_local_plugin_fails_to_import() {
    let workspace = ConfigWorkspace::new();
    let plugins = workspace.plugins_dir();
    let file = plugins.join("greeter.ts");
    let setup = workspace.project.join("setup.log");
    let close = workspace.project.join("close.log");
    write(&file, &effect_plugin("greeter", &setup, Some(&close)));

    let Some(harness) = Harness::start(workspace).await else { return };
    harness.registry.await_activation().await;
    assert!(active_ids(&harness.registry).contains(&"greeter".to_string()));

    write(&file, "export default {");
    harness.watcher.emit();
    wait_until(
        || {
            harness.registry.list().iter().any(|info| {
                matches!(info.state, PluginState::Failed { .. }) && matches!(&info.source, Source::Local { .. })
            })
        },
        "the broken revision is reported failed",
    )
    .await;
    assert!(active_ids(&harness.registry).contains(&"greeter".to_string()), "the previous generation keeps running");
    assert!(read(&close).is_empty(), "the previous activation is not torn down");

    write(&file, &(effect_plugin("greeter", &setup, Some(&close)) + "// fixed\n"));
    harness.watcher.emit();
    wait_until(|| read(&setup).lines().count() == 2, "the fixed plugin reactivates").await;
    assert!(active_ids(&harness.registry).contains(&"greeter".to_string()));

    harness.shutdown().await;
}

#[tokio::test]
async fn applies_organization_integration_changes_to_running_external_plugins() {
    let workspace = ConfigWorkspace::new();
    let root = workspace.project.join("external").join("policy-fixture");
    let entered = workspace.project.join("entered");
    let setup = workspace.project.join("setup.log");
    write(
        &root.join("index.ts"),
        &format!(
            "await Bun.write({}, \"loaded\")\nimport {{ Effect }} from \"effect\"\nimport {{ appendFileSync }} from \"node:fs\"\nexport default {{\n  id: \"greeter\",\n  effect: () => Effect.gen(function* () {{ appendFileSync({}, \"setup\\n\") }}),\n}}\n",
            js(path_string(&entered)),
            js(path_string(&setup))
        ),
    );
    let target = path_string(&root);
    config(
        &workspace,
        json!({
            "plugins": [target],
            "policies": [{ "action": "plugin.use", "resource": "*", "effect": "deny" }],
        }),
    );

    let Some(harness) = Harness::start(workspace).await else { return };
    harness.registry.await_activation().await;
    assert!(!entered.exists(), "a denied plugin must not be imported");

    config(&harness.workspace, json!({ "plugins": [target] }));
    harness.config.reload().await;
    harness.watcher.emit();
    wait_until(
        || entered.exists() && active_ids(&harness.registry).contains(&"greeter".to_string()),
        "the allowed plugin runs",
    )
    .await;

    let policy = json!({
        "policies": [{ "action": "plugin.use", "resource": target, "effect": "deny" }],
    });
    harness
        .config
        .integration(
            "opencode",
            Some(IntegrationDocument {
                connection: json!({ "type": "env", "name": "OPENCODE_API_KEY" }),
                organization: None,
                info: parse_info(&policy.to_string(), "opencode").info.expect("the policy document parses"),
            }),
        )
        .await;
    wait_until(
        || !harness.registry.list().iter().any(|info| info.id.as_deref() == Some("greeter")),
        "the organization policy removes the plugin",
    )
    .await;

    harness.config.integration("opencode", None).await;
    wait_until(
        || active_ids(&harness.registry).contains(&"greeter".to_string()),
        "removing the organization policy brings the plugin back",
    )
    .await;

    harness.shutdown().await;
}

#[tokio::test]
async fn closing_a_location_during_a_pending_load_does_not_hang() {
    let workspace = ConfigWorkspace::new();
    let plugins = workspace.plugins_dir();
    let entered = workspace.project.join("entered");
    write(
        &plugins.join("pending.ts"),
        &format!(
            "await Bun.write({}, \"entered\")\nawait new Promise(() => {{}})\nexport default {{ id: \"pending\", async setup() {{}} }}\n",
            js(path_string(&entered))
        ),
    );

    let Some(harness) = Harness::start(workspace).await else { return };
    wait_until(|| entered.exists(), "the plugin import started").await;

    tokio::time::timeout(Duration::from_secs(5), harness.shutdown())
        .await
        .expect("closing a location during a pending load returns");
}

#[tokio::test]
async fn reactivates_script_plugins_after_a_sidecar_restart() {
    let workspace = ConfigWorkspace::new();
    let plugins = workspace.plugins_dir();
    let setup = workspace.project.join("setup.log");
    let close = workspace.project.join("close.log");
    write(&plugins.join("greeter.ts"), &effect_plugin("greeter", &setup, Some(&close)));

    let Some(harness) = Harness::start_with(workspace, Duration::from_millis(400)).await else { return };
    harness.registry.await_activation().await;
    assert_eq!(read(&setup).lines().count(), 1);

    let _ = harness.sidecar.call(Request::new("host.crash", json!({ "code": 0 }))).await;
    wait_until(
        || active_ids(&harness.registry).contains(&"greeter".to_string()) && read(&setup).lines().count() >= 2,
        "the plugin reactivates after the crash",
    )
    .await;
    assert_eq!(read(&setup).lines().count(), 2, "one crash triggers exactly one reactivation");

    harness.shutdown().await;
}
