use std::path::{Path, PathBuf};

use opencode_config::watch::IGNORED_DIRECTORIES;

const MAX_DEPTH: usize = 6;

pub fn ignore() -> Vec<String> {
    let mut globs = crate::scan::ignore_globs();
    globs.push("**/.*".to_string());
    globs.push("**/.*/**".to_string());
    globs
}

pub fn scan(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut queue = vec![(root.to_path_buf(), 0usize)];
    let mut index = 0;
    while index < queue.len() {
        let (directory, depth) = queue[index].clone();
        index += 1;
        let Ok(entries) = std::fs::read_dir(&directory) else { continue };
        let children: Vec<(String, PathBuf, Kind)> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') || IGNORED_DIRECTORIES.contains(&name.as_str()) {
                    return None;
                }
                let path = directory.join(&name);
                let kind = match std::fs::metadata(&path) {
                    Ok(metadata) if metadata.is_file() => Kind::File,
                    Ok(metadata) if metadata.is_dir() => Kind::Directory,
                    _ => Kind::Unknown,
                };
                Some((name, path, kind))
            })
            .collect();
        if let Some((_, path, _)) = children.iter().find(|(name, _, kind)| *kind == Kind::File && name == "SKILL.md") {
            files.push(path.clone());
            continue;
        }
        if depth == 0 {
            files.extend(
                children
                    .iter()
                    .filter(|(name, _, kind)| *kind == Kind::File && name.ends_with(".md"))
                    .map(|(_, path, _)| path.clone()),
            );
        }
        if depth < MAX_DEPTH {
            queue.extend(
                children
                    .into_iter()
                    .filter(|(_, _, kind)| *kind == Kind::Directory)
                    .map(|(_, path, _)| (path, depth + 1)),
            );
        }
    }
    files.sort_by(|a, b| a.to_string_lossy().cmp(&b.to_string_lossy()));
    files
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    Directory,
    Unknown,
}
