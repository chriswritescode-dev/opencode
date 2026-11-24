use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use opencode_util::path::Flavor;

pub enum Source<'a> {
    Path(&'a Path),
    Virtual { source: &'a str, dir: &'a Path },
}

pub struct SubstituteInput<'a> {
    pub source: Source<'a>,
    pub text: &'a str,
    pub env: Option<&'a BTreeMap<String, String>>,
    pub home: &'a Path,
}

const ENV_PREFIX: &str = "{env:";
const FILE_PREFIX: &str = "{file:";

pub fn substitute(
    input: &SubstituteInput<'_>,
    references: &mut BTreeSet<PathBuf>,
    var: impl Fn(&str) -> Option<String>,
) -> String {
    let raw = substitute_files(input, &substitute_env(input, &var, false), references);
    if !input.text.contains(ENV_PREFIX) || opencode_local::jsonc::parse_tree(&raw).errors == 0 {
        return raw;
    }
    substitute_files(input, &substitute_env(input, &var, true), references)
}

fn substitute_env(input: &SubstituteInput<'_>, var: &impl Fn(&str) -> Option<String>, escape: bool) -> String {
    let text = input.text;
    let mut out = String::new();
    let mut cursor = 0;
    let mut search = 0;
    while let Some(offset) = text[search..].find(ENV_PREFIX) {
        let start = search + offset;
        let Some(end) = text[start + ENV_PREFIX.len()..].find('}') else { break };
        let close = start + ENV_PREFIX.len() + end;
        let name = &text[start + ENV_PREFIX.len()..close];
        if name.is_empty() {
            search = start + 1;
            continue;
        }
        out.push_str(&text[cursor..start]);
        let value = input.env.and_then(|env| env.get(name).cloned()).or_else(|| var(name)).unwrap_or_default();
        let rendered = if escape { json_escape(&value) } else { value };
        out.push_str(&rendered);
        cursor = close + 1;
        search = cursor;
    }
    out.push_str(&text[cursor..]);
    out
}

fn substitute_files(input: &SubstituteInput<'_>, text: &str, references: &mut BTreeSet<PathBuf>) -> String {
    let dir = source_dir(&input.source);
    let home = input.home.to_string_lossy();
    let mut out = String::new();
    let mut cursor = 0;
    let mut search = 0;
    while let Some(offset) = text[search..].find(FILE_PREFIX) {
        let start = search + offset;
        let Some(end) = text[start + FILE_PREFIX.len()..].find('}') else { break };
        let close = start + FILE_PREFIX.len() + end;
        let file_path = &text[start + FILE_PREFIX.len()..close];
        if file_path.is_empty() {
            search = start + 1;
            continue;
        }
        out.push_str(&text[cursor..start]);

        let line_start = text[..start].rfind('\n').map_or(0, |index| index + 1);
        if text[line_start..start].trim_start().starts_with("//") {
            out.push_str(&text[start..=close]);
            cursor = close + 1;
            search = cursor;
            continue;
        }

        let expanded = match file_path.strip_prefix("~/") {
            Some(rest) => Flavor::NATIVE.join(&[home.as_ref(), rest]),
            None => file_path.to_owned(),
        };
        let resolved = PathBuf::from(Flavor::NATIVE.resolve(&[&dir, &expanded]));
        let resolved = match std::env::current_dir() {
            Ok(directory) if !resolved.is_absolute() => directory.join(&resolved),
            _ => resolved,
        };
        references.insert(resolved.clone());
        let content = match read_file(&resolved) {
            Ok(content) => content,
            Err(error) => {
                log::warn!(
                    "config file reference is missing or unreadable source={} reference={} path={} error={error}",
                    source_name(&input.source),
                    &text[start..=close],
                    resolved.display()
                );
                String::new()
            }
        };
        out.push_str(&json_escape(content.trim()));
        cursor = close + 1;
        search = cursor;
    }
    out.push_str(&text[cursor..]);
    out
}

fn source_dir(source: &Source<'_>) -> String {
    match source {
        Source::Path(path) => match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_string_lossy().into_owned(),
            _ => ".".to_owned(),
        },
        Source::Virtual { dir, .. } => dir.to_string_lossy().into_owned(),
    }
}

fn source_name(source: &Source<'_>) -> String {
    match source {
        Source::Path(path) => path.to_string_lossy().into_owned(),
        Source::Virtual { source, .. } => (*source).to_owned(),
    }
}

fn read_file(path: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    let text = String::from_utf8_lossy(&bytes);
    Ok(text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned())
}

fn json_escape(value: &str) -> String {
    let quoted = serde_json::Value::String(value.to_owned()).to_string();
    quoted.get(1..quoted.len() - 1).unwrap_or_default().to_owned()
}
