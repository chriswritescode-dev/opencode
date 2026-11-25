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
    let ignore: Vec<Regex> =
        options.ignore.iter().flat_map(|pattern| compile(pattern).into_iter().map(|pattern| pattern.regex)).collect();
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
            if std::fs::metadata(&candidate).is_ok_and(|metadata| metadata.is_file()) {
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

struct Pattern {
    regex: Regex,
    segments: Vec<Segment>,
}

enum Segment {
    Globstar,
    Part { matcher: Regex, literal: Option<String> },
}

fn walk(
    directory: &Path,
    prefix: &str,
    patterns: &[Pattern],
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
        let symlink = entry.file_type().is_ok_and(|file| file.is_symlink());
        let is_directory = matches!(kind, Kind::Directory);
        let reported_directory = is_directory && !(symlink && !options.symlink);
        if patterns.iter().any(|pattern| pattern.regex.is_match(&relative))
            && (!reported_directory || !options.include_files_only)
        {
            results.push(path.clone());
        }
        if !is_directory {
            continue;
        }
        if symlink && !options.symlink && !patterns.iter().any(|pattern| reaches_literal(&pattern.segments, &relative))
        {
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

fn reaches_literal(segments: &[Segment], relative: &str) -> bool {
    let parts: Vec<&str> = relative.split('/').collect();
    reaches_at(segments, &parts, 0, 0)
}

fn reaches_at(segments: &[Segment], parts: &[&str], segment: usize, part: usize) -> bool {
    let Some(current) = segments.get(segment) else { return false };
    if matches!(current, Segment::Globstar) {
        if reaches_at(segments, parts, segment + 1, part) {
            return true;
        }
        return part + 1 < parts.len() && reaches_at(segments, parts, segment, part + 1);
    }
    let Some(name) = parts.get(part) else { return false };
    let Segment::Part { matcher, literal } = current else { return false };
    if part + 1 == parts.len() {
        return literal.as_deref() == Some(*name);
    }
    matcher.is_match(name) && reaches_at(segments, parts, segment + 1, part + 1)
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

fn compile(pattern: &str) -> Vec<Pattern> {
    let normalized = pattern.replace('\\', "/");
    expand_braces(&normalized)
        .into_iter()
        .filter_map(|pattern| {
            let regex = Regex::new(&translate(&pattern)).ok()?;
            let segments = pattern
                .split('/')
                .map(|segment| {
                    if segment == "**" {
                        return Some(Segment::Globstar);
                    }
                    let matcher = Regex::new(&format!("^{}$", translate_segment(segment))).ok()?;
                    Some(Segment::Part { matcher, literal: literal_segment(segment) })
                })
                .collect::<Option<Vec<_>>>()?;
            Some(Pattern { regex, segments })
        })
        .collect()
}

fn literal_segment(segment: &str) -> Option<String> {
    let literal = !segment.bytes().any(|byte| matches!(byte, b'*' | b'?' | b'[' | b'{'));
    literal.then(|| segment.to_owned())
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
