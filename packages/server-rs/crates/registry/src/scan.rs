use std::path::{Path, PathBuf};

use opencode_config::watch::IGNORED_DIRECTORIES;
use regex_lite::Regex;

pub fn ignore_globs() -> Vec<String> {
    let mut globs: Vec<String> = IGNORED_DIRECTORIES.iter().map(|name| (*name).to_owned()).collect();
    globs.push(format!("**/{{{}}}/**", IGNORED_DIRECTORIES.join(",")));
    globs
}

#[derive(Clone, Debug)]
pub struct ScanOptions<'a> {
    pub dot: bool,
    pub symlink: bool,
    pub include_files_only: bool,
    pub ignore: &'a [&'a str],
}

impl Default for ScanOptions<'_> {
    fn default() -> Self {
        Self { dot: false, symlink: false, include_files_only: true, ignore: &[] }
    }
}

pub fn scan(root: &Path, pattern: &str, options: ScanOptions<'_>) -> Vec<PathBuf> {
    if !root.is_dir() {
        return Vec::new();
    }
    let patterns = compile(pattern);
    if patterns.is_empty() {
        return Vec::new();
    }
    let ignore: Vec<Regex> = options.ignore.iter().flat_map(|pattern| compile(pattern)).collect();
    let mut results = Vec::new();
    let mut ancestors = Vec::new();
    if let Ok(path) = root.canonicalize() {
        ancestors.push(path);
    }
    walk(root, "", &patterns, &ignore, &options, &mut ancestors, &mut results);
    results.sort_by(|a, b| a.to_string_lossy().cmp(&b.to_string_lossy()));
    results
}

pub fn up(names: &[&str], start: &Path, stop: Option<&Path>) -> Vec<PathBuf> {
    let mut results = Vec::new();
    let mut current = start.to_path_buf();
    loop {
        for name in names {
            let candidate = current.join(name);
            if std::fs::symlink_metadata(&candidate).is_ok() {
                results.push(candidate);
            }
        }
        if stop.is_some_and(|stop| same_directory(stop, &current)) {
            break;
        }
        let Some(parent) = current.parent() else { break };
        if parent == current {
            break;
        }
        current = parent.to_path_buf();
    }
    results
}

pub fn glob_up(pattern: &str, start: &Path, stop: Option<&Path>) -> Vec<PathBuf> {
    let mut results = Vec::new();
    let mut current = start.to_path_buf();
    loop {
        results.extend(scan(
            &current,
            pattern,
            ScanOptions { dot: true, include_files_only: true, ..Default::default() },
        ));
        if stop.is_some_and(|stop| same_directory(stop, &current)) {
            break;
        }
        let Some(parent) = current.parent() else { break };
        if parent == current {
            break;
        }
        current = parent.to_path_buf();
    }
    results
}

#[derive(Clone, Copy)]
enum Kind {
    File,
    Directory,
}

fn walk(
    directory: &Path,
    prefix: &str,
    patterns: &[Regex],
    ignore: &[Regex],
    options: &ScanOptions<'_>,
    ancestors: &mut Vec<PathBuf>,
    results: &mut Vec<PathBuf>,
) {
    let Ok(entries) = std::fs::read_dir(directory) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !options.dot && name.starts_with('.') {
            continue;
        }
        let relative = if prefix.is_empty() { name.into_owned() } else { format!("{prefix}/{name}") };
        if ignored(&relative, ignore) {
            continue;
        }
        let Some(kind) = entry_kind(&path) else { continue };
        if patterns.iter().any(|pattern| pattern.is_match(&relative))
            && (!matches!(kind, Kind::Directory) || !options.include_files_only)
        {
            results.push(path.clone());
        }
        if !matches!(kind, Kind::Directory) {
            continue;
        }
        let symlink = entry.file_type().is_ok_and(|file| file.is_symlink());
        if symlink && !options.symlink {
            continue;
        }
        let Ok(canonical) = path.canonicalize() else { continue };
        if ancestors.contains(&canonical) {
            continue;
        }
        ancestors.push(canonical);
        walk(&path, &relative, patterns, ignore, options, ancestors, results);
        ancestors.pop();
    }
}

fn entry_kind(path: &Path) -> Option<Kind> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    let metadata = if metadata.file_type().is_symlink() { std::fs::metadata(path).ok()? } else { metadata };
    if metadata.is_dir() {
        Some(Kind::Directory)
    } else if metadata.is_file() {
        Some(Kind::File)
    } else {
        None
    }
}

fn ignored(relative: &str, patterns: &[Regex]) -> bool {
    if patterns.is_empty() {
        return false;
    }
    let children = format!("{relative}/");
    patterns.iter().any(|pattern| pattern.is_match(relative) || pattern.is_match(&children))
}

fn same_directory(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn compile(pattern: &str) -> Vec<Regex> {
    let normalized = pattern.replace('\\', "/");
    expand_braces(&normalized).into_iter().filter_map(|pattern| Regex::new(&translate(&pattern)).ok()).collect()
}

fn translate(pattern: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();
    for segment in pattern.split('/') {
        if segment == "**" && segments.last() == Some(&"**") {
            continue;
        }
        segments.push(segment);
    }
    let mut regex = String::from("^");
    let mut first = true;
    let mut separator = false;
    for (index, segment) in segments.iter().enumerate() {
        let last = index + 1 == segments.len();
        if *segment == "**" {
            if !first {
                regex.push('/');
            }
            if last {
                regex.push_str(".*");
            } else {
                regex.push_str("(?:[^/]+/)*");
            }
            first = false;
            separator = false;
            continue;
        }
        if !first && separator {
            regex.push('/');
        }
        regex.push_str(&translate_segment(segment));
        first = false;
        separator = true;
    }
    regex.push('$');
    regex
}

fn translate_segment(segment: &str) -> String {
    let characters: Vec<char> = segment.chars().collect();
    let mut output = String::new();
    let mut index = 0;
    while index < characters.len() {
        match characters[index] {
            '*' => {
                output.push_str("[^/]*");
                index += 1;
            }
            '?' => {
                output.push_str("[^/]");
                index += 1;
            }
            '[' => {
                let Some(close) = class_end(&characters, index) else {
                    escape('[', &mut output);
                    index += 1;
                    continue;
                };
                output.push('[');
                let mut start = index + 1;
                if characters.get(start) == Some(&'!') {
                    output.push('^');
                    start += 1;
                } else if characters.get(start) == Some(&'^') {
                    output.push_str("\\^");
                    start += 1;
                }
                for character in &characters[start..close] {
                    match character {
                        '\\' | ']' | '[' | '^' => {
                            output.push('\\');
                            output.push(*character);
                        }
                        other => output.push(*other),
                    }
                }
                output.push(']');
                index = close + 1;
            }
            character => {
                escape(character, &mut output);
                index += 1;
            }
        }
    }
    output
}

fn class_end(characters: &[char], open: usize) -> Option<usize> {
    let mut index = open + 1;
    if matches!(characters.get(index), Some('!' | '^')) {
        index += 1;
    }
    if characters.get(index) == Some(&']') {
        index += 1;
    }
    while index < characters.len() {
        if characters[index] == ']' {
            return Some(index);
        }
        index += 1;
    }
    None
}

fn escape(character: char, output: &mut String) {
    if matches!(character, '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$') {
        output.push('\\');
    }
    output.push(character);
}

fn expand_braces(pattern: &str) -> Vec<String> {
    let characters: Vec<char> = pattern.chars().collect();
    expand(&characters)
}

fn expand(characters: &[char]) -> Vec<String> {
    let mut in_class = false;
    let mut index = 0;
    while index < characters.len() {
        match characters[index] {
            '[' => in_class = true,
            ']' => in_class = false,
            '{' if !in_class => {
                let Some(close) = brace_end(characters, index) else {
                    index += 1;
                    continue;
                };
                let mut parts = Vec::new();
                let mut current = Vec::new();
                let mut depth = 0;
                let mut class = false;
                for character in &characters[index + 1..close] {
                    match character {
                        '[' => {
                            class = true;
                            current.push(*character);
                        }
                        ']' => {
                            class = false;
                            current.push(*character);
                        }
                        '{' if !class => {
                            depth += 1;
                            current.push(*character);
                        }
                        '}' if !class => {
                            depth -= 1;
                            current.push(*character);
                        }
                        ',' if !class && depth == 0 => parts.push(std::mem::take(&mut current)),
                        other => current.push(*other),
                    }
                }
                parts.push(current);
                let prefix = &characters[..index];
                let suffix = &characters[close + 1..];
                let mut result = Vec::new();
                for part in parts {
                    let mut combined = prefix.to_vec();
                    combined.extend(part);
                    combined.extend_from_slice(suffix);
                    result.extend(expand(&combined));
                }
                return result;
            }
            _ => {}
        }
        index += 1;
    }
    vec![characters.iter().collect()]
}

fn brace_end(characters: &[char], open: usize) -> Option<usize> {
    let mut depth = 0;
    let mut in_class = false;
    for (index, character) in characters.iter().enumerate().skip(open) {
        match character {
            '[' => in_class = true,
            ']' => in_class = false,
            '{' if !in_class => depth += 1,
            '}' if !in_class => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}
