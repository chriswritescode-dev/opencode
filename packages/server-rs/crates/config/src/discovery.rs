use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use opencode_util::path::{Flavor, realpath, windows_path};

use crate::options::{ConfigLocation, ConfigOptions, SystemOption};
use crate::paths::GlobalPaths;

pub const NAMES: [&str; 2] = ["opencode.json", "opencode.jsonc"];

const CANDIDATES: [&str; 5] = [".claude", ".agents", ".opencode", "opencode.jsonc", "opencode.json"];
const ECOSYSTEM: [&str; 3] = [".agents", ".claude", ".opencode"];
const PREFERENCE_METADATA: [&str; 6] =
    ["PayloadDisplayName", "PayloadIdentifier", "PayloadType", "PayloadUUID", "PayloadVersion", "_manualProfile"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Root {
    pub path: PathBuf,
    pub present: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sources {
    pub system: Option<Root>,
    pub preferences: Option<PathBuf>,
    pub global: Option<PathBuf>,
    pub explicit: Option<PathBuf>,
    pub direct: Vec<PathBuf>,
    pub project: Vec<Root>,
    pub claude: Vec<PathBuf>,
    pub agents: Vec<PathBuf>,
}

pub fn discover(options: &ConfigOptions, location: &ConfigLocation, paths: &GlobalPaths) -> Sources {
    let system = match &options.system {
        Some(SystemOption::Disabled) => None,
        Some(SystemOption::Path(path)) => Some(resolve_plain(path)),
        None => Some(system_directory()),
    };
    let global = paths.config.clone();
    let global_agents = joined(&paths.home, ".agents");
    let global_claude = joined(&paths.home, ".claude");
    let mut config_directories: Vec<PathBuf> = Vec::new();
    if let Some(system) = &system {
        config_directories.push(system.clone());
    }
    config_directories.push(global.clone());

    let global_roots: Vec<PathBuf> = config_directories
        .iter()
        .chain([&global_claude, &global_agents])
        .map(|directory| resolve_path(directory))
        .collect();
    let current = resolve_path(&location.directory);
    let directories = if global_roots[..config_directories.len()].contains(&current) || options.project == Some(false) {
        Vec::new()
    } else {
        upward(&location.directory)
    };

    let mut discovered = Vec::new();
    for directory in &directories {
        let parent = if directory == &location.directory { current.clone() } else { resolve_path(directory) };
        let entries: Option<HashSet<String>> = std::fs::read_dir(&parent).ok().map(|listing| {
            listing.filter_map(Result::ok).map(|entry| entry.file_name().to_string_lossy().to_lowercase()).collect()
        });
        for name in CANDIDATES {
            let item = joined(directory, name);
            let missing = entries.as_ref().is_some_and(|entries| !entries.contains(&name.to_lowercase()));
            let resolved = if missing { plain_resolve(&parent, name) } else { resolve_path(&joined(&parent, name)) };
            discovered.push(Entry { item, resolved, missing });
        }
    }

    let global_enabled = options.global != Some(false);
    let global_files: Vec<PathBuf> = config_directories
        .iter()
        .flat_map(|directory| NAMES.iter().map(move |name| resolve_path(&joined(directory, name))))
        .collect();
    let visible: Vec<&Entry> = discovered
        .iter()
        .filter(|entry| {
            if global_enabled {
                return !global_roots.contains(&entry.resolved) && !global_files.contains(&entry.resolved);
            }
            !global_roots.iter().any(|root| entry.resolved == *root || entry.resolved.starts_with(root))
        })
        .collect();

    let preferences = preferences(options, global_enabled);

    let system = if global_enabled { system.map(|path| Root { present: path.is_dir(), path }) } else { None };
    let explicit = options.file.as_ref().filter(|path| !path.as_os_str().is_empty()).map(|path| resolve_plain(path));
    let direct = visible
        .iter()
        .rev()
        .filter(|entry| !ECOSYSTEM.contains(&basename(&entry.item).as_str()))
        .map(|entry| entry.item.clone())
        .collect();
    let project = visible
        .iter()
        .rev()
        .filter(|entry| basename(&entry.item) == ".opencode")
        .map(|entry| Root { present: !entry.missing && entry.item.is_dir(), path: entry.item.clone() })
        .collect();
    let claude = if options.claude == Some(false) {
        Vec::new()
    } else {
        let mut claude = Vec::new();
        if global_enabled {
            claude.push(global_claude);
        }
        claude.extend(
            visible.iter().rev().filter(|entry| basename(&entry.item) == ".claude").map(|entry| entry.item.clone()),
        );
        dedupe(claude)
    };
    let mut agents = Vec::new();
    if global_enabled {
        agents.push(global_agents);
    }
    agents.extend(
        visible.iter().rev().filter(|entry| basename(&entry.item) == ".agents").map(|entry| entry.item.clone()),
    );
    Sources {
        system,
        preferences,
        global: global_enabled.then_some(global),
        explicit,
        direct,
        project,
        claude,
        agents: dedupe(agents),
    }
}

pub fn overlay(options: &ConfigOptions, location: &ConfigLocation, paths: &GlobalPaths) -> Option<Root> {
    if options.global == Some(false) || options.project == Some(false) || location.project_id == "global" {
        return None;
    }
    let path = joined(&joined(&paths.config, "project"), &location.project_id);
    Some(Root { present: path.is_dir(), path })
}

pub fn system_directory() -> PathBuf {
    if cfg!(target_os = "macos") {
        return PathBuf::from("/Library/Application Support/opencode");
    }
    if cfg!(windows) {
        let base = std::env::var_os("ProgramData")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| OsString::from("C:\\ProgramData"));
        return PathBuf::from(Flavor::NATIVE.join(&[&base.to_string_lossy(), "opencode"]));
    }
    PathBuf::from("/etc/opencode")
}

pub fn preference_paths(user: &str) -> Vec<PathBuf> {
    let base = PathBuf::from("/Library/Managed Preferences");
    vec![base.join(user).join("ai.opencode.managed.plist"), base.join("ai.opencode.managed.plist")]
}

pub fn strip_preference_metadata(json: &str) -> String {
    let mut object = match serde_json::from_str::<serde_json::Value>(json) {
        Ok(serde_json::Value::Object(object)) => object,
        _ => return json.to_owned(),
    };
    for key in PREFERENCE_METADATA {
        object.remove(key);
    }
    serde_json::to_string(&serde_json::Value::Object(object)).unwrap_or_else(|_| json.to_owned())
}

struct Entry {
    item: PathBuf,
    resolved: PathBuf,
    missing: bool,
}

fn preferences(options: &ConfigOptions, global_enabled: bool) -> Option<PathBuf> {
    if !global_enabled || options.system.is_some() {
        return None;
    }
    if !cfg!(target_os = "macos") {
        return None;
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "user".to_owned());
    preference_paths(&user).into_iter().find(|path| path.is_file())
}

fn upward(start: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut current = start.to_path_buf();
    loop {
        let search = joined(&current, ".");
        if search.exists() {
            found.push(search);
        }
        let next = match current.parent() {
            Some(parent) if parent.as_os_str().is_empty() => PathBuf::from("."),
            Some(parent) => parent.to_path_buf(),
            None => break,
        };
        if next == current {
            break;
        }
        current = next;
    }
    found
}

fn dedupe(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    paths.into_iter().filter(|path| seen.insert(path.clone())).collect()
}

fn basename(path: &Path) -> String {
    Flavor::NATIVE.basename(&path.to_string_lossy()).to_owned()
}

fn joined(parent: &Path, name: &str) -> PathBuf {
    PathBuf::from(Flavor::NATIVE.join(&[&parent.to_string_lossy(), name]))
}

fn plain_resolve(parent: &Path, name: &str) -> PathBuf {
    resolve_plain(&joined(parent, name))
}

fn resolve_plain(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    let flavored = windows_path(Flavor::NATIVE, &text);
    let resolved = PathBuf::from(Flavor::NATIVE.resolve(&[&flavored]));
    if resolved.is_absolute() {
        return resolved;
    }
    match std::env::current_dir() {
        Ok(directory) => directory.join(&resolved),
        Err(_) => resolved,
    }
}

fn resolve_path(path: &Path) -> PathBuf {
    let absolute = resolve_plain(path);
    realpath(&absolute).unwrap_or(absolute)
}
