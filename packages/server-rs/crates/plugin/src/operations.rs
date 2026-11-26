use std::path::{Component, Path, PathBuf};

use opencode_config::entry::Entry;
use opencode_config::policy::{Action, Effect, decision, statements};
use opencode_contract::generated::ConfigEntryDocumentInfoPluginsItem;
use serde::Serialize;

use crate::generation::Source;
use crate::host::{Entrypoints, ScriptHost};

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Operation {
    Add(AddOperation),
    Remove { target: String },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AddOperation {
    pub target: String,
    pub options: serde_json::Map<String, serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtime: Option<i64>,
}

pub fn parse(item: &ConfigEntryDocumentInfoPluginsItem) -> Result<Operation, String> {
    match item {
        ConfigEntryDocumentInfoPluginsItem::String(value) => {
            if !value.starts_with('-') {
                return Ok(Operation::Add(AddOperation {
                    target: value.clone(),
                    options: serde_json::Map::new(),
                    mtime: None,
                }));
            }
            if value == "-" {
                return Err("Plugin remove operation requires a target".to_string());
            }
            Ok(Operation::Remove { target: value[1..].to_string() })
        }
        ConfigEntryDocumentInfoPluginsItem::Object(object) => Ok(Operation::Add(AddOperation {
            target: object.package.clone(),
            options: object
                .options
                .as_ref()
                .map_or_else(serde_json::Map::new, |options| options.0.iter().cloned().collect()),
            mtime: None,
        })),
    }
}

pub fn discover(directory: &Path) -> Vec<PathBuf> {
    let mut children = ["plugin", "plugins"]
        .iter()
        .flat_map(|name| {
            std::fs::read_dir(directory.join(name))
                .map(|entries| {
                    entries
                        .filter_map(Result::ok)
                        .filter_map(|entry| Some((entry.path(), entry.file_type().ok()?)))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    children.sort_by(|left, right| left.0.as_os_str().as_encoded_bytes().cmp(right.0.as_os_str().as_encoded_bytes()));
    children
        .into_iter()
        .filter_map(|(path, file_type)| {
            if file_type.is_file() {
                return is_source(&path).then_some(path);
            }
            if file_type.is_dir() {
                return Some(path);
            }
            if !file_type.is_symlink() {
                return None;
            }
            if is_source(&path) && path.is_file() {
                return Some(path);
            }
            path.is_dir().then_some(path)
        })
        .collect()
}

pub fn configured(entries: &[Entry], location_directory: &Path) -> Vec<Operation> {
    entries
        .iter()
        .flat_map(|entry| match entry {
            Entry::Document(document) => document
                .info
                .plugins
                .iter()
                .flatten()
                .filter_map(|item| configured_operation(item, document.path.as_deref(), location_directory))
                .collect::<Vec<_>>(),
            Entry::Directory { .. } => Vec::new(),
        })
        .collect()
}

pub async fn scan(entries: &[Entry], location_directory: &Path, host: Option<&dyn ScriptHost>) -> Vec<Operation> {
    let mut operations = entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Directory { path } => Some(discover(path)),
            Entry::Document(_) => None,
        })
        .flatten()
        .map(|target| {
            Operation::Add(AddOperation {
                target: target.to_string_lossy().into_owned(),
                options: serde_json::Map::new(),
                mtime: None,
            })
        })
        .collect::<Vec<_>>();
    operations.extend(configured(entries, location_directory));
    let mut resolved = Vec::with_capacity(operations.len());
    for operation in operations {
        if let Some(operation) = resolve_operation(operation, host).await {
            resolved.push(operation);
        }
    }
    resolved
}

pub fn policy_blocks(entries: &[Entry], target: &str) -> bool {
    let name = if Path::new(target).is_absolute() {
        target
    } else {
        target.rfind('@').filter(|index| *index > 0).map_or(target, |index| &target[..index])
    };
    decision(&statements(entries), Action::PluginUse, name) == Effect::Deny
}

pub fn source_of(target: &str, version: Option<&str>) -> Source {
    if Path::new(target).is_absolute() {
        return Source::Local { path: target.to_string() };
    }
    Source::Package {
        target: target.to_string(),
        version: version.map(str::to_string),
        outdated: false,
        updating: false,
    }
}

async fn resolve_operation(operation: Operation, host: Option<&dyn ScriptHost>) -> Option<Operation> {
    let add = match operation {
        Operation::Remove { target } => return Some(Operation::Remove { target }),
        Operation::Add(add) => add,
    };
    let target = Path::new(&add.target);
    if !target.is_absolute() || !target.exists() {
        return Some(Operation::Add(add));
    }
    let directory = std::fs::metadata(target).map(|metadata| metadata.is_dir()).unwrap_or(false);
    let entrypoints = if directory {
        match host {
            Some(host) => host.resolve_local(target.to_path_buf()).await.unwrap_or_default(),
            None => Entrypoints::default(),
        }
    } else {
        Entrypoints { server: file_url(target), ..Default::default() }
    };
    let entrypoint = entrypoints.server.clone().or_else(|| entrypoints.gui.clone())?;
    if directory {
        let Ok(root) = std::fs::canonicalize(target) else {
            return None;
        };
        let path = file_path(&entrypoint)?;
        let Ok(resolved) = std::fs::canonicalize(path) else {
            return None;
        };
        if !resolved.starts_with(&root) {
            return None;
        }
    }
    let package_json = if directory {
        Some(target.join("package.json"))
    } else {
        target.parent().map(|parent| parent.join("package.json"))
    };
    let mtime = [
        entrypoints.server.as_deref(),
        entrypoints.tui.as_deref(),
        entrypoints.rpc.as_deref(),
        entrypoints.gui.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter_map(file_path)
    .chain(package_json)
    .map(|path| mtime_ms(&path))
    .max()
    .unwrap_or(0);
    Some(Operation::Add(AddOperation { target: add.target, options: add.options, mtime: Some(mtime) }))
}

fn configured_operation(
    item: &ConfigEntryDocumentInfoPluginsItem,
    document: Option<&Path>,
    location_directory: &Path,
) -> Option<Operation> {
    let operation = match parse(item) {
        Ok(operation) => operation,
        Err(message) => {
            log::warn!("{message}");
            return None;
        }
    };
    let add = match operation {
        Operation::Remove { target } => return Some(Operation::Remove { target }),
        Operation::Add(add) => add,
    };
    let target = configured_target(add.target, document, location_directory);
    let path = Path::new(&target);
    if path.is_absolute() && path.is_file() {
        log::warn!("configured plugin path must be a directory");
        return None;
    }
    Some(Operation::Add(AddOperation { target, options: add.options, mtime: add.mtime }))
}

fn configured_target(target: String, document: Option<&Path>, location_directory: &Path) -> String {
    if target.starts_with("file://") {
        return url::Url::parse(&target)
            .ok()
            .and_then(|url| url.to_file_path().ok())
            .map_or(target, |path| path.to_string_lossy().into_owned());
    }
    if target.starts_with("./") || target.starts_with("../") {
        return resolve_lexically(document.and_then(Path::parent).unwrap_or(location_directory), &target);
    }
    target
}

fn resolve_lexically(base: &Path, target: &str) -> String {
    base.join(target)
        .components()
        .fold(PathBuf::new(), |mut resolved, component| {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    resolved.pop();
                }
                other => resolved.push(other),
            }
            resolved
        })
        .to_string_lossy()
        .into_owned()
}

fn is_source(path: &Path) -> bool {
    let bytes = path.as_os_str().as_encoded_bytes();
    bytes.ends_with(b".ts") || bytes.ends_with(b".js")
}

fn file_url(path: &Path) -> Option<String> {
    url::Url::from_file_path(path).ok().map(|url| url.to_string())
}

fn file_path(value: &str) -> Option<PathBuf> {
    url::Url::parse(value).ok().and_then(|url| url.to_file_path().ok())
}

fn mtime_ms(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}
