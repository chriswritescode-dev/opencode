use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use opencode_config::discovery::{
    Root, Sources, discover, overlay, preference_paths, strip_preference_metadata, system_directory,
};
use opencode_config::options::{ConfigLocation, ConfigOptions, SystemOption, truthy};
use opencode_config::paths::GlobalPaths;
use opencode_util::temp::TempDir;
use serde_json::{Value, json};

use crate::support::area;

#[test]
fn every_discover_case_matches_the_pinned_typescript() {
    let cases = area("discover");
    assert_eq!(cases.len(), 21, "expected 21 discover cases, found {}", cases.len());
    'cases: for case in &cases {
        let temp = TempDir::new().unwrap();
        let mut permissions = ModeGuard::new();
        let root = opencode_util::path::realpath(temp.path()).unwrap();

        if let Some(directories) = case.value.get("directories").and_then(Value::as_array) {
            for directory in directories {
                std::fs::create_dir_all(root.join(directory.as_str().unwrap())).unwrap();
            }
        }
        if let Some(files) = case.value.get("files").and_then(Value::as_object) {
            for (path, content) in files {
                let target = root.join(path);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).unwrap();
                }
                std::fs::write(&target, content.as_str().unwrap()).unwrap();
            }
        }
        if let Some(symlinks) = case.value.get("symlinks").and_then(Value::as_object) {
            for (link, target) in symlinks {
                let link = root.join(link);
                if let Some(parent) = link.parent() {
                    std::fs::create_dir_all(parent).unwrap();
                }
                if let Err(error) = create_symlink(&root.join(target.as_str().unwrap()), &link) {
                    if !cfg!(unix) {
                        println!("skipping {}: cannot create symlink: {error}", case.name);
                        continue 'cases;
                    }
                    panic!("{} symlink {}: {error}", case.name, link.display());
                }
            }
        }
        if let Some(modes) = case.value.get("modes").and_then(Value::as_object) {
            for (path, mode) in modes {
                let mode = mode.as_u64().unwrap_or_else(|| panic!("{} mode for {path} is not a number", case.name));
                permissions.apply(&root.join(path), mode as u32);
            }
        }

        let global = case.value.get("global").unwrap_or_else(|| panic!("{} global is missing", case.name));
        let home = root.join(global["home"].as_str().unwrap());
        let config = root.join(global["config"].as_str().unwrap());
        let paths = GlobalPaths::from_vars(|name| match name {
            "HOME" => Some(home.clone().into_os_string()),
            "OPENCODE_CONFIG_DIR" => Some(config.clone().into_os_string()),
            _ => None,
        })
        .unwrap();

        let options = config_options(
            &root,
            case.value.get("options").unwrap_or_else(|| panic!("{} options is missing", case.name)),
        );
        let location_value = case.value.get("location").unwrap_or_else(|| panic!("{} location is missing", case.name));
        let location = ConfigLocation {
            directory: root.join(location_value["directory"].as_str().unwrap()),
            project_id: location_value["project_id"].as_str().unwrap().to_owned(),
        };

        let sources = discover(&options, &location, &paths);
        assert_eq!(sources_view(&root, &sources), case.value["sources"], "{} sources", case.name);

        let overlay = overlay(&options, &location, &paths);
        assert_eq!(
            json!(overlay.as_ref().and_then(|item| root_view(&root, item))),
            case.value["overlay"],
            "{} overlay",
            case.name
        );
    }
}

#[test]
fn strip_preference_metadata_removes_payload_keys() {
    let input = json!({
        "PayloadDisplayName": "OpenCode",
        "PayloadIdentifier": "ai.opencode.managed",
        "PayloadType": "ai.opencode.managed",
        "PayloadUUID": "00000000-0000-0000-0000-000000000000",
        "PayloadVersion": 1,
        "_manualProfile": true,
        "share": "disabled",
    })
    .to_string();
    assert_eq!(strip_preference_metadata(&input), r#"{"share":"disabled"}"#);
    for unchanged in [r#"[1,2]"#, r#""text""#, "null", r#"{"unclosed":]"#] {
        assert_eq!(strip_preference_metadata(unchanged), unchanged);
    }
}

#[test]
fn preference_paths_are_user_then_global() {
    assert_eq!(
        preference_paths("alice"),
        vec![
            PathBuf::from("/Library/Managed Preferences/alice/ai.opencode.managed.plist"),
            PathBuf::from("/Library/Managed Preferences/ai.opencode.managed.plist"),
        ]
    );
}

#[test]
fn from_vars_maps_server_process_variables() {
    let defaults = options_from(&[]);
    assert_eq!(defaults.project, Some(true));
    assert_eq!(defaults.claude, Some(true));
    assert_eq!(defaults.file, None);
    assert_eq!(defaults.content, None);
    assert_eq!(defaults.global, None);
    assert_eq!(defaults.system, None);

    assert_eq!(options_from(&[("OPENCODE_CONFIG_PROJECT_DISABLE", "1")]).project, Some(false));
    assert_eq!(options_from(&[("OPENCODE_DISABLE_PROJECT_CONFIG", "true")]).project, Some(false));
    assert_eq!(
        options_from(&[("OPENCODE_CONFIG_PROJECT_DISABLE", "0"), ("OPENCODE_DISABLE_PROJECT_CONFIG", "1")]).project,
        Some(true)
    );
    assert_eq!(options_from(&[("OPENCODE_DISABLE_CLAUDE_CODE", "1")]).claude, Some(false));
    assert_eq!(options_from(&[("OPENCODE_DISABLE_CLAUDE_CODE_SKILLS", "TRUE")]).claude, Some(false));
    assert_eq!(
        options_from(&[("OPENCODE_DISABLE_CLAUDE_CODE", "0"), ("OPENCODE_DISABLE_CLAUDE_CODE_SKILLS", "0")]).claude,
        Some(true)
    );

    let explicit = options_from(&[
        ("OPENCODE_CONFIG", "/tmp/custom.json"),
        ("OPENCODE_CONFIG_CONTENT", "{\"share\":\"disabled\"}"),
    ]);
    assert_eq!(explicit.file, Some(PathBuf::from("/tmp/custom.json")));
    assert_eq!(explicit.content.as_deref(), Some("{\"share\":\"disabled\"}"));

    assert!(truthy(Some(OsStr::new("true"))));
    assert!(truthy(Some(OsStr::new("TRUE"))));
    assert!(truthy(Some(OsStr::new("1"))));
    assert!(!truthy(Some(OsStr::new("yes"))));
    assert!(!truthy(Some(OsStr::new("0"))));
    assert!(!truthy(None));
}

#[test]
#[cfg(all(unix, not(target_os = "macos")))]
fn system_directory_is_platform_specific() {
    assert_eq!(system_directory(), PathBuf::from("/etc/opencode"));
}

#[test]
#[cfg(target_os = "macos")]
fn system_directory_is_platform_specific() {
    assert_eq!(system_directory(), PathBuf::from("/Library/Application Support/opencode"));
}

struct ModeGuard {
    #[cfg(unix)]
    paths: Vec<PathBuf>,
}

impl ModeGuard {
    #[cfg(unix)]
    fn new() -> Self {
        Self { paths: Vec::new() }
    }

    #[cfg(not(unix))]
    fn new() -> Self {
        Self {}
    }

    fn apply(&mut self, path: &Path, mode: u32) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
            self.paths.push(path.to_path_buf());
        }
        #[cfg(not(unix))]
        let _ = (path, mode);
    }
}

impl Drop for ModeGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in &self.paths {
                let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
            }
        }
    }
}

#[cfg(unix)]
fn create_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn create_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link).or_else(|_| std::os::windows::fs::symlink_file(target, link))
}

fn options_from(vars: &[(&str, &str)]) -> ConfigOptions {
    let vars: HashMap<String, OsString> =
        vars.iter().map(|(key, value)| ((*key).to_owned(), OsString::from(*value))).collect();
    ConfigOptions::from_vars(|name| vars.get(name).cloned())
}

fn config_options(root: &Path, value: &Value) -> ConfigOptions {
    let system = match value.get("system") {
        Some(Value::Bool(false)) => Some(SystemOption::Disabled),
        Some(Value::String(path)) => Some(SystemOption::Path(root.join(path))),
        _ => None,
    };
    ConfigOptions {
        project: value.get("project").and_then(Value::as_bool),
        global: value.get("global").and_then(Value::as_bool),
        system,
        claude: value.get("claude").and_then(Value::as_bool),
        file: value.get("file").and_then(Value::as_str).map(|path| root.join(path)),
        content: value.get("content").and_then(Value::as_str).map(str::to_owned),
    }
}

fn sources_view(root: &Path, sources: &Sources) -> Value {
    json!({
        "system": sources.system.as_ref().and_then(|item| root_view(root, item)),
        "preferences": sources.preferences.as_deref().and_then(|path| relative(root, path)),
        "global": sources.global.as_deref().and_then(|path| relative(root, path)),
        "explicit": sources.explicit.as_deref().and_then(|path| relative(root, path)),
        "direct": relative_list(root, &sources.direct),
        "project": sources.project.iter().filter_map(|item| root_view(root, item)).collect::<Vec<_>>(),
        "claude": relative_list(root, &sources.claude),
        "agents": relative_list(root, &sources.agents),
    })
}

fn root_view(root: &Path, item: &Root) -> Option<Value> {
    let path = relative(root, &item.path)?;
    Some(json!({ "path": path, "present": item.present }))
}

fn relative_list(root: &Path, paths: &[PathBuf]) -> Vec<String> {
    paths.iter().filter_map(|path| relative(root, path)).collect()
}

fn relative(root: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(root).ok().map(|relative| relative.to_string_lossy().replace('\\', "/"))
}
