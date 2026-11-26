#![cfg(unix)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use opencode_config::options::{ConfigLocation, ConfigOptions, SystemOption};
use opencode_config::paths::GlobalPaths;
use opencode_config::seams::NoKv;
use opencode_config::service::{ConfigService, ServiceDeps};
use opencode_config::shell::{self, Priority, Search, ShellSelect};
use opencode_config::wellknown::WellKnown;
use opencode_contract::generated::requests::ConfigPatch;
use opencode_util::temp::TempDir;
use serde_json::Value;

#[test]
fn select_corpus_matches() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let home = root.join("home");
    fixture(root);

    for case in corpus() {
        let name = case["name"].as_str().unwrap();
        let kind = case["kind"].as_str().unwrap();
        let file = case["shell"].as_str().map(|value| substitute(root, &home, value));
        match kind {
            "login" => {
                assert_eq!(shell::login(file.as_deref().unwrap()), case["expected"].as_bool().unwrap(), "{name}")
            }
            "ps" => assert_eq!(shell::ps(file.as_deref().unwrap()), case["expected"].as_bool().unwrap(), "{name}"),
            "name" => assert_eq!(shell::name(file.as_deref().unwrap()), case["expected"].as_str().unwrap(), "{name}"),
            "args" => {
                let expected: Vec<String> = case["expected"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|value| value.as_str().unwrap().to_owned())
                    .collect();
                assert_eq!(
                    shell::args(file.as_deref().unwrap(), case["command"].as_str().unwrap()),
                    expected,
                    "{name}"
                );
            }
            "resolve" => {
                let search = Search {
                    bin: PathBuf::from(
                        case["bin"].as_str().map(|value| substitute(root, &home, value)).unwrap_or_default(),
                    ),
                    path: Some(OsString::from(substitute(root, &home, case["path"].as_str().unwrap_or("")))),
                    gitbash: None,
                };
                let shell_value = file.clone();
                let vars = move |key: &str| match (key, &shell_value) {
                    ("SHELL", Some(value)) => Some(OsString::from(value)),
                    _ => None,
                };
                let select = ShellSelect::new(search, vars);
                if let Some(configured) = case["configured"].as_str() {
                    select.configure(Some(&substitute(root, &home, configured)));
                }
                let priority = match case["priority"].as_str().unwrap() {
                    "config" => Priority::Config,
                    _ => Priority::Compat,
                };
                assert_eq!(
                    select.resolve(priority),
                    substitute(root, &home, case["expected"].as_str().unwrap()),
                    "{name}"
                );
            }
            other => panic!("{name}: unknown corpus kind {other}"),
        }
    }
}

#[test]
fn configure_clears_the_cached_resolution() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let bin = root.join("bin");
    let bash = bin.join("bash");
    let zsh = bin.join("zsh");
    std::fs::create_dir_all(&bin).unwrap();
    executable(&bash);
    executable(&zsh);

    let select = ShellSelect::new(
        Search { bin: PathBuf::new(), path: Some(bin.clone().into_os_string()), gitbash: None },
        |_| None,
    );
    assert_eq!(select.resolve(Priority::Config), bash.to_string_lossy());
    std::fs::remove_file(&bash).unwrap();
    assert_eq!(select.resolve(Priority::Config), bash.to_string_lossy(), "the first resolution is cached");

    select.configure(Some(&zsh.to_string_lossy()));
    assert_eq!(select.resolve(Priority::Config), zsh.to_string_lossy(), "the configured shell wins");

    select.configure(None);
    assert_eq!(select.resolve(Priority::Config), "/bin/sh", "configure clears the cache");
}

#[tokio::test]
async fn observe_config_applies_the_configured_shell_and_clears_it_on_reload() {
    let temp = TempDir::new().unwrap();
    let root = opencode_util::path::realpath(temp.path()).unwrap();
    let home = root.join("home");
    let global = root.join("global");
    let project = root.join("project");
    let bin = root.join("bin");
    for directory in [&home, &global, &project, &bin] {
        std::fs::create_dir_all(directory).unwrap();
    }
    let bash = bin.join("bash");
    let zsh = bin.join("zsh");
    executable(&bash);
    executable(&zsh);
    std::fs::write(global.join("opencode.jsonc"), format!("{{\n  \"shell\": \"{}\"\n}}\n", zsh.display())).unwrap();

    let paths = GlobalPaths::from_vars(|name| match name {
        "HOME" => Some(home.clone().into_os_string()),
        "OPENCODE_CONFIG_DIR" => Some(global.clone().into_os_string()),
        _ => None,
    })
    .unwrap();
    let location = ConfigLocation { directory: project, project_id: "global".to_owned() };
    let deps = ServiceDeps {
        credentials: Arc::new(()),
        wellknown: Arc::new(WellKnown::new(Arc::new(NoKv))),
        channel: "unknown".to_owned(),
        flag_overrides: None,
    };
    let service = ConfigService::start(
        ConfigOptions {
            project: Some(false),
            global: None,
            system: Some(SystemOption::Disabled),
            claude: Some(false),
            file: None,
            content: None,
        },
        location,
        paths,
        deps,
    )
    .await;

    let select = Arc::new(ShellSelect::new(
        Search { bin: PathBuf::new(), path: Some(bin.into_os_string()), gitbash: None },
        |_| None,
    ));
    select.observe_config(&service);
    assert_eq!(select.resolve(Priority::Config), zsh.to_string_lossy());

    service.update(&ConfigPatch { shell: None }).await.unwrap();
    let mut resolved = None;
    for _ in 0..1000 {
        let value = select.resolve(Priority::Config);
        if value != zsh.to_string_lossy() {
            resolved = Some(value);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(resolved.expect("the config reload clears the configured shell"), bash.to_string_lossy());
}

fn corpus() -> Vec<Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("corpus").join("shell-select.json");
    let input =
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    let value: Value =
        serde_json::from_str(&input).unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()));
    value.as_array().cloned().unwrap_or_else(|| panic!("{} is not a JSON array", path.display()))
}

fn substitute(root: &Path, home: &Path, value: &str) -> String {
    value.replace("{{home}}", &home.to_string_lossy()).replace("{{root}}", &root.to_string_lossy())
}

fn fixture(root: &Path) {
    for directory in [root.join("bin"), root.join("global"), root.join("empty")] {
        std::fs::create_dir_all(directory).unwrap();
    }
    for name in ["bash", "zsh", "sh", "dash", "fish", "nu", "pwsh", "powershell", "cmd", "cmd.exe"] {
        executable(&root.join("bin").join(name));
    }
    executable(&root.join("global").join("bash"));
}

#[cfg(unix)]
fn executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, "").unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}
