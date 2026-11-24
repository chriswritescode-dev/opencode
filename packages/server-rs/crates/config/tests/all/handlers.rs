use std::path::{Path, PathBuf};
use std::sync::Arc;

use opencode_config::handlers;
use opencode_config::options::{ConfigLocation, ConfigOptions, SystemOption};
use opencode_config::paths::GlobalPaths;
use opencode_config::seams::NoKv;
use opencode_config::service::{ConfigService, ServiceDeps};
use opencode_config::wellknown::WellKnown;
use opencode_contract::codec::{Encode, encode_json};
use opencode_contract::generated::requests::ConfigPatch;
use opencode_util::temp::TempDir;
use serde_json::Value;

use crate::support::{area, relative};

struct Workspace {
    _temp: TempDir,
    root: PathBuf,
    home: PathBuf,
    global: PathBuf,
    project: PathBuf,
}

fn workspace() -> Workspace {
    let temp = TempDir::new().unwrap();
    let root = opencode_util::path::realpath(temp.path()).unwrap();
    let home = root.join("home");
    let global = root.join("global");
    let project = root.join("project");
    for directory in [&home, &global, &project] {
        std::fs::create_dir_all(directory).unwrap();
    }
    Workspace { _temp: temp, root, home, global, project }
}

async fn service(workspace: &Workspace, options: ConfigOptions) -> ConfigService {
    let paths = GlobalPaths::from_vars(|name| match name {
        "HOME" => Some(workspace.home.clone().into_os_string()),
        "OPENCODE_CONFIG_DIR" => Some(workspace.global.clone().into_os_string()),
        _ => None,
    })
    .unwrap();
    let location = ConfigLocation { directory: workspace.project.clone(), project_id: "global".to_owned() };
    let deps = ServiceDeps {
        credentials: Arc::new(()),
        wellknown: Arc::new(WellKnown::new(Arc::new(NoKv))),
        channel: "unknown".to_owned(),
        flag_overrides: None,
    };
    ConfigService::start(options, location, paths, deps).await
}

#[tokio::test]
async fn config_get_returns_the_ordered_load_corpus_entries() {
    let case = area("load")
        .into_iter()
        .find(|case| case.name == "json-and-jsonc-order")
        .expect("the load corpus holds json-and-jsonc-order");
    let workspace = workspace();
    for directory in case.value["directories"].as_array().cloned().unwrap_or_default() {
        std::fs::create_dir_all(workspace.root.join(directory.as_str().unwrap())).unwrap();
    }
    for (path, content) in case.value["files"].as_object().cloned().unwrap_or_default() {
        let target = workspace.root.join(&path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&target, content.as_str().unwrap()).unwrap();
    }
    let service = service(
        &workspace,
        ConfigOptions {
            project: None,
            global: None,
            system: Some(SystemOption::Disabled),
            claude: None,
            file: None,
            content: None,
        },
    )
    .await;

    let entries: Vec<Value> = value(&handlers::config_get(&service))
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|mut entry| {
            if let Some(path) = entry.get("path").and_then(Value::as_str).map(str::to_owned) {
                entry["path"] = Value::String(relative(&workspace.root, Path::new(&path)));
            }
            entry
        })
        .collect();
    let expected: Vec<Value> = case.value["entries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|entry| {
            let mut object = entry.as_object().cloned().expect("a stored entry is an object");
            let _ = object.remove("origin");
            Value::Object(object)
        })
        .collect();
    assert_eq!(entries, expected, "{} entries", case.name);
}

#[tokio::test]
async fn experimental_config_update_keeps_jsonc_and_reloads() {
    let workspace = workspace();
    let file = workspace.global.join("opencode.jsonc");
    std::fs::write(&file, "{\n  // keep this comment\n  \"model\": \"provider/model\",\n  \"shell\": \"bash\"\n}\n")
        .unwrap();
    let service = service(
        &workspace,
        ConfigOptions {
            project: Some(false),
            global: None,
            system: Some(SystemOption::Disabled),
            claude: Some(false),
            file: None,
            content: None,
        },
    )
    .await;

    let mut updated = service.subscribe_updated();
    handlers::experimental_config_update(&service, ConfigPatch { shell: Some("/bin/zsh".to_owned()) }).await.unwrap();

    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("// keep this comment"), "the comment survives the update");
    assert!(text.contains("\"model\": \"provider/model\""), "the model survives the update");
    assert!(text.contains("\"shell\": \"/bin/zsh\""), "the shell is replaced");
    tokio::time::timeout(std::time::Duration::from_secs(10), updated.recv())
        .await
        .expect("a config.updated event arrives")
        .expect("the config service is alive");

    let path = file.to_string_lossy().into_owned();
    let entries = value(&handlers::config_get(&service));
    let document = entries
        .as_array()
        .expect("config entries are an array")
        .iter()
        .find(|entry| entry.get("path").and_then(Value::as_str) == Some(path.as_str()))
        .expect("the updated document is an entry");
    assert_eq!(document["info"]["shell"].as_str(), Some("/bin/zsh"));
}

fn value<T: Encode + ?Sized>(value: &T) -> Value {
    serde_json::from_str(&encode_json(value)).expect("the wire encoding is JSON")
}

#[cfg(unix)]
mod unix {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use opencode_config::handlers;
    use opencode_config::shell;
    use opencode_contract::generated::ConfigShellOption;
    use opencode_util::temp::TempDir;

    fn fixture() -> (TempDir, PathBuf, PathBuf) {
        let root = TempDir::new().unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let bash = bin.join("bash");
        let fish = bin.join("fish");
        for file in [&bash, &fish] {
            std::fs::write(file, "").unwrap();
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let missing = bin.join("missing");
        let shells = root.path().join("shells");
        let text = format!(
            "# a comment\n\n{}\n{}\n{}\n{}\n",
            bash.display(),
            fish.display(),
            missing.display(),
            bash.display()
        );
        std::fs::write(&shells, text).unwrap();
        (root, bin, shells)
    }

    #[test]
    fn lists_executable_shells_once_and_marks_denied_ones() {
        let (_root, bin, shells) = fixture();
        assert_eq!(
            shell::list(&shells, Some(bin.as_os_str()), Path::new(""), None),
            vec![
                shell::Item {
                    path: bin.join("bash").to_string_lossy().into_owned(),
                    name: "bash".to_owned(),
                    acceptable: true,
                },
                shell::Item {
                    path: bin.join("fish").to_string_lossy().into_owned(),
                    name: "fish".to_owned(),
                    acceptable: false,
                },
            ]
        );
    }

    #[test]
    fn name_falls_back_to_the_item_path_without_a_path_match() {
        let (root, bin, shells) = fixture();
        let empty = root.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let items = shell::list(&shells, Some(empty.as_os_str()), Path::new(""), None);
        assert_eq!(
            items,
            vec![
                shell::Item {
                    path: bin.join("bash").to_string_lossy().into_owned(),
                    name: bin.join("bash").to_string_lossy().into_owned(),
                    acceptable: true,
                },
                shell::Item {
                    path: bin.join("fish").to_string_lossy().into_owned(),
                    name: bin.join("fish").to_string_lossy().into_owned(),
                    acceptable: false,
                },
            ]
        );
    }

    #[test]
    fn config_shells_with_maps_the_fixture() {
        let (_root, bin, shells) = fixture();
        assert_eq!(
            handlers::config_shells_with(&bin, None, &shells, Some(bin.as_os_str())),
            vec![
                ConfigShellOption {
                    path: bin.join("bash").to_string_lossy().into_owned(),
                    name: "bash".to_owned(),
                    acceptable: true,
                },
                ConfigShellOption {
                    path: bin.join("fish").to_string_lossy().into_owned(),
                    name: "fish".to_owned(),
                    acceptable: false,
                },
            ]
        );
    }
}
