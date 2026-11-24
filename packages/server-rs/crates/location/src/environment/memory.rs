use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard};

use futures_util::future::BoxFuture;

use super::{
    CommandOutput, CommandSpec, DirEntry, Environment, EnvironmentError, FileInfo, FileRange, FileRead, FileType,
    Files, Spawner,
};

#[derive(Clone, Debug)]
enum MemoryNode {
    File { bytes: Vec<u8>, mtime_ms: f64 },
    Directory { mtime_ms: f64 },
    Symlink { target: String, mtime_ms: f64 },
}

impl MemoryNode {
    fn kind(&self) -> FileType {
        match self {
            MemoryNode::File { .. } => FileType::File,
            MemoryNode::Directory { .. } => FileType::Directory,
            MemoryNode::Symlink { .. } => FileType::Symlink,
        }
    }

    fn file_bytes(&self) -> Option<&[u8]> {
        match self {
            MemoryNode::File { bytes, .. } => Some(bytes),
            _ => None,
        }
    }

    fn mtime_ms(&self) -> f64 {
        match self {
            MemoryNode::File { mtime_ms, .. }
            | MemoryNode::Directory { mtime_ms }
            | MemoryNode::Symlink { mtime_ms, .. } => *mtime_ms,
        }
    }

    fn info(&self) -> FileInfo {
        let size = match self {
            MemoryNode::File { bytes, .. } => bytes.len() as u64,
            MemoryNode::Symlink { target, .. } => target.len() as u64,
            MemoryNode::Directory { .. } => 0,
        };
        FileInfo { kind: self.kind(), size, mtime_ms: self.mtime_ms() }
    }
}

pub struct MemoryFiles {
    nodes: Mutex<HashMap<String, MemoryNode>>,
}

impl MemoryFiles {
    pub fn new() -> Self {
        let mut nodes = HashMap::new();
        nodes.insert("/".to_string(), MemoryNode::Directory { mtime_ms: now_ms() });
        MemoryFiles { nodes: Mutex::new(nodes) }
    }

    pub fn symlink(&self, target: &str, path: &str) -> Result<(), EnvironmentError> {
        let mut nodes = self.lock();
        symlink(&mut nodes, target, path).map_err(|cause| EnvironmentError::failed(path, cause))
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, MemoryNode>> {
        self.nodes.lock().unwrap_or_else(|error| error.into_inner())
    }
}

impl Default for MemoryFiles {
    fn default() -> Self {
        MemoryFiles::new()
    }
}

impl Files for MemoryFiles {
    fn read<'a>(
        &'a self,
        path: &'a str,
        range: Option<FileRange>,
    ) -> BoxFuture<'a, Result<FileRead, EnvironmentError>> {
        Box::pin(async move {
            let nodes = self.lock();
            read(&nodes, path, range)
        })
    }

    fn write<'a>(&'a self, path: &'a str, bytes: &'a [u8]) -> BoxFuture<'a, Result<(), EnvironmentError>> {
        Box::pin(async move {
            let mut nodes = self.lock();
            write(&mut nodes, path, bytes).map_err(|cause| EnvironmentError::failed(path, cause))
        })
    }

    fn stat<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<FileInfo, EnvironmentError>> {
        Box::pin(async move { stat(&self.lock(), path) })
    }

    fn list<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Vec<DirEntry>, EnvironmentError>> {
        Box::pin(async move { list(&self.lock(), path) })
    }

    fn remove<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), EnvironmentError>> {
        Box::pin(async move {
            let mut nodes = self.lock();
            remove(&mut nodes, path);
            Ok(())
        })
    }

    fn rename<'a>(&'a self, from: &'a str, to: &'a str) -> BoxFuture<'a, Result<(), EnvironmentError>> {
        Box::pin(async move {
            let mut nodes = self.lock();
            rename(&mut nodes, from, to)
        })
    }

    fn mkdir<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), EnvironmentError>> {
        Box::pin(async move {
            let mut nodes = self.lock();
            mkdir(&mut nodes, path).map_err(|cause| EnvironmentError::failed(path, cause))
        })
    }

    fn append<'a>(&'a self, path: &'a str, bytes: &'a [u8]) -> BoxFuture<'a, Result<(), EnvironmentError>> {
        Box::pin(async move {
            let mut nodes = self.lock();
            append(&mut nodes, path, bytes).map_err(|cause| EnvironmentError::failed(path, cause))
        })
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MemorySpawner;

impl MemorySpawner {
    pub fn new() -> Self {
        MemorySpawner
    }
}

impl Spawner for MemorySpawner {
    fn spawn<'a>(&'a self, command: &'a CommandSpec) -> BoxFuture<'a, Result<CommandOutput, EnvironmentError>> {
        Box::pin(async move {
            Err(EnvironmentError::failed(command.program.clone(), "The memory driver cannot spawn processes"))
        })
    }
}

pub struct MemoryEnvironment {
    id: String,
    files: MemoryFiles,
    spawner: MemorySpawner,
}

impl MemoryEnvironment {
    pub fn new(id: impl Into<String>) -> Self {
        MemoryEnvironment { id: id.into(), files: MemoryFiles::new(), spawner: MemorySpawner::new() }
    }

    pub fn symlink(&self, target: &str, path: &str) -> Result<(), EnvironmentError> {
        self.files.symlink(target, path)
    }
}

impl Environment for MemoryEnvironment {
    fn id(&self) -> &str {
        &self.id
    }

    fn files(&self) -> &dyn Files {
        &self.files
    }

    fn spawner(&self) -> &dyn Spawner {
        &self.spawner
    }

    fn hosts_server(&self) -> bool {
        false
    }

    fn searchable(&self) -> bool {
        false
    }
}

fn stat(nodes: &HashMap<String, MemoryNode>, path: &str) -> Result<FileInfo, EnvironmentError> {
    match lookup(nodes, path) {
        Some(node) => Ok(node.info()),
        None => Err(EnvironmentError::not_found(path)),
    }
}

fn read(
    nodes: &HashMap<String, MemoryNode>,
    path: &str,
    range: Option<FileRange>,
) -> Result<FileRead, EnvironmentError> {
    let Some(original) = lookup(nodes, path) else {
        return Err(EnvironmentError::not_found(path));
    };
    if original.kind() == FileType::Directory {
        return Err(EnvironmentError::WrongKind { path: path.to_string(), actual: FileType::Directory });
    }
    let resolved = resolve_key(nodes, path, true, &mut HashSet::new());
    let Some(node) = resolved.and_then(|key| nodes.get(&key)) else {
        return Err(EnvironmentError::not_found(path));
    };
    let Some(bytes) = node.file_bytes() else {
        return Err(EnvironmentError::WrongKind { path: path.to_string(), actual: node.kind() });
    };
    let bytes = match range {
        Some(range) => slice(bytes, range),
        None => bytes.to_vec(),
    };
    Ok(FileRead { info: node.info(), bytes })
}

fn list(nodes: &HashMap<String, MemoryNode>, path: &str) -> Result<Vec<DirEntry>, EnvironmentError> {
    let target = resolve_key(nodes, path, true, &mut HashSet::new()).unwrap_or_else(|| normalize(path));
    let Some(node) = nodes.get(&target) else {
        return Err(EnvironmentError::not_found(path));
    };
    if node.kind() != FileType::Directory {
        return Err(EnvironmentError::WrongKind { path: path.to_string(), actual: node.kind() });
    }
    let mut entries: Vec<DirEntry> = nodes
        .iter()
        .filter(|(entry, _)| entry.as_str() != target.as_str() && parent(entry.as_str()) == target)
        .map(|(entry, child)| DirEntry { name: basename(entry.as_str()).to_string(), kind: child.kind() })
        .collect();
    entries.sort_by_key(|entry| entry.name.clone());
    Ok(entries)
}

fn write(nodes: &mut HashMap<String, MemoryNode>, path: &str, bytes: &[u8]) -> Result<(), String> {
    mkdir(nodes, &parent(&normalize(path)))?;
    let existing = lookup(nodes, path);
    if existing.as_ref().is_some_and(|node| node.kind() == FileType::Directory) {
        return Err(format!("Path is a directory: {path}"));
    }
    let follow = existing.as_ref().is_some_and(|node| node.kind() == FileType::Symlink);
    let target = resolve_key(nodes, path, follow, &mut HashSet::new())
        .ok_or_else(|| format!("Cannot resolve symlink: {path}"))?;
    require_parent(nodes, &target)?;
    nodes.insert(target, MemoryNode::File { bytes: bytes.to_vec(), mtime_ms: now_ms() });
    Ok(())
}

fn remove(nodes: &mut HashMap<String, MemoryNode>, path: &str) {
    let target = resolve_key(nodes, path, false, &mut HashSet::new()).unwrap_or_else(|| normalize(path));
    let prefix = format!("{target}/");
    nodes.retain(|entry, _| entry != &target && !entry.starts_with(&prefix));
}

fn rename(nodes: &mut HashMap<String, MemoryNode>, from: &str, to: &str) -> Result<(), EnvironmentError> {
    let source = resolve_key(nodes, from, false, &mut HashSet::new()).unwrap_or_else(|| normalize(from));
    if !nodes.contains_key(&source) {
        return Err(EnvironmentError::not_found(from));
    }
    move_node(nodes, &source, to).map_err(|cause| EnvironmentError::failed(from, cause))
}

fn mkdir(nodes: &mut HashMap<String, MemoryNode>, value: &str) -> Result<(), String> {
    let target = resolve_key(nodes, value, false, &mut HashSet::new()).unwrap_or_else(|| normalize(value));
    let existing = nodes.get(&target).map(MemoryNode::kind);
    if existing == Some(FileType::Directory) {
        return Ok(());
    }
    if existing.is_some() {
        return Err(format!("Path is not a directory: {value}"));
    }
    let directory = parent(&target);
    if directory != target {
        mkdir(nodes, &directory)?;
    }
    nodes.insert(target, MemoryNode::Directory { mtime_ms: now_ms() });
    Ok(())
}

fn append(nodes: &mut HashMap<String, MemoryNode>, path: &str, bytes: &[u8]) -> Result<(), String> {
    let target = resolve_key(nodes, path, true, &mut HashSet::new()).unwrap_or_else(|| normalize(path));
    let current = nodes.get(&target).cloned();
    if current.as_ref().is_some_and(|node| node.kind() != FileType::File) {
        return Err(format!("Path is not a file: {path}"));
    }
    if current.is_none() {
        require_parent(nodes, &target)?;
    }
    let mut next = current.and_then(|node| node.file_bytes().map(|bytes| bytes.to_vec())).unwrap_or_default();
    next.extend_from_slice(bytes);
    nodes.insert(target, MemoryNode::File { bytes: next, mtime_ms: now_ms() });
    Ok(())
}

fn symlink(nodes: &mut HashMap<String, MemoryNode>, target: &str, path: &str) -> Result<(), String> {
    require_parent(nodes, path)?;
    let key = resolve_key(nodes, path, false, &mut HashSet::new()).unwrap_or_else(|| normalize(path));
    nodes.insert(key, MemoryNode::Symlink { target: target.to_string(), mtime_ms: now_ms() });
    Ok(())
}

fn move_node(nodes: &mut HashMap<String, MemoryNode>, source: &str, to: &str) -> Result<(), String> {
    let requested = resolve_key(nodes, to, false, &mut HashSet::new()).unwrap_or_else(|| normalize(to));
    let into_directory = nodes.get(&requested).map(MemoryNode::kind) == Some(FileType::Directory);
    let destination = if into_directory { join(&requested, basename(source)) } else { requested };
    let directory = nodes.get(source).map(MemoryNode::kind) == Some(FileType::Directory);
    if directory && destination.starts_with(&format!("{source}/")) {
        return Err(format!("Cannot move a directory into itself: {source}"));
    }
    let existing = nodes.get(&destination).map(MemoryNode::kind);
    if directory && existing.is_some() && existing != Some(FileType::Directory) {
        return Err(format!("Cannot overwrite a non-directory with a directory: {to}"));
    }
    require_parent(nodes, &destination)?;
    let prefix = format!("{source}/");
    let moved: Vec<(String, MemoryNode)> = nodes
        .iter()
        .filter(|(entry, _)| entry.as_str() == source || entry.starts_with(&prefix))
        .map(|(entry, child)| (entry.clone(), child.clone()))
        .collect();
    for (entry, _) in &moved {
        nodes.remove(entry);
    }
    for (entry, child) in moved {
        nodes.insert(format!("{destination}{}", &entry[source.len()..]), child);
    }
    Ok(())
}

fn require_parent(nodes: &HashMap<String, MemoryNode>, value: &str) -> Result<(), String> {
    let parent_path = parent(&normalize(value));
    let resolved = resolve_key(nodes, &parent_path, true, &mut HashSet::new()).unwrap_or_else(|| parent_path.clone());
    let Some(parent) = nodes.get(&resolved) else {
        return Err(format!("Parent directory does not exist: {parent_path}"));
    };
    if parent.kind() != FileType::Directory {
        return Err(format!("Parent is not a directory: {parent_path}"));
    }
    Ok(())
}

fn normalize(value: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in value.split('/') {
        match part {
            "" | "." => continue,
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        return "/".to_string();
    }
    format!("/{}", parts.join("/"))
}

fn parent(path: &str) -> String {
    match path.rfind('/') {
        Some(index) if index > 0 => path[..index].to_string(),
        _ => "/".to_string(),
    }
}

fn basename(path: &str) -> &str {
    match path.rfind('/') {
        Some(index) => &path[index + 1..],
        None => path,
    }
}

fn join(base: &str, name: &str) -> String {
    normalize(&format!("{base}/{name}"))
}

fn resolve_key(
    nodes: &HashMap<String, MemoryNode>,
    value: &str,
    follow_final: bool,
    seen: &mut HashSet<String>,
) -> Option<String> {
    let normalized = normalize(value);
    let parts: Vec<&str> = normalized.split('/').filter(|part| !part.is_empty()).collect();
    walk(nodes, "/", &parts, follow_final, seen)
}

fn walk(
    nodes: &HashMap<String, MemoryNode>,
    current: &str,
    parts: &[&str],
    follow_final: bool,
    seen: &mut HashSet<String>,
) -> Option<String> {
    let Some((part, rest)) = parts.split_first() else {
        return Some(current.to_string());
    };
    let candidate = join(current, part);
    let target = match nodes.get(&candidate) {
        Some(MemoryNode::Symlink { target, .. }) if follow_final || !rest.is_empty() => target.clone(),
        _ => return walk(nodes, &candidate, rest, follow_final, seen),
    };
    if !seen.insert(candidate.clone()) {
        return None;
    }
    let resolved = if target.starts_with('/') {
        normalize(&target)
    } else {
        normalize(&format!("{}/{}", parent(&candidate), target))
    };
    let next = if rest.is_empty() { resolved } else { join(&resolved, &rest.join("/")) };
    resolve_key(nodes, &next, follow_final, seen)
}

fn lookup(nodes: &HashMap<String, MemoryNode>, value: &str) -> Option<MemoryNode> {
    let key = resolve_key(nodes, value, false, &mut HashSet::new()).unwrap_or_else(|| normalize(value));
    nodes.get(&key).cloned()
}

fn slice(bytes: &[u8], range: FileRange) -> Vec<u8> {
    let start = usize::try_from(range.offset).unwrap_or(usize::MAX).min(bytes.len());
    let length = usize::try_from(range.length).unwrap_or(usize::MAX);
    let end = start.saturating_add(length).min(bytes.len());
    bytes[start..end].to_vec()
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0)
}
