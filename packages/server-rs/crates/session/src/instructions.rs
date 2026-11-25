use std::fmt;

use opencode_contract::codec::encode_json;
use opencode_util::hash::sha256_hex;
use opencode_util::time::{Zone, now_ms};
use serde_json::Value;

pub enum Observed {
    Value(Value),
    Unavailable,
    Removed,
}

pub struct ReadResult {
    pub key: String,
    pub value: Observed,
}

type Initializer = Box<dyn Fn(&Value) -> Option<String> + Send + Sync>;
type Changer = Box<dyn Fn(&Value, &Value) -> Option<String> + Send + Sync>;
type Remover = Box<dyn Fn(&Value) -> Option<String> + Send + Sync>;

pub struct Source {
    pub key: String,
    pub read: Box<dyn Fn() -> Observed + Send + Sync>,
    pub initial: Initializer,
    pub changed: Changer,
    pub removed: Option<Remover>,
}

pub type List = Vec<Source>;

pub type Values = Vec<(String, String)>;

pub struct Admission {
    pub delta: Vec<(String, Option<String>)>,
    pub blobs: Vec<(String, Value)>,
}

pub struct Rendered {
    pub key: String,
    pub text: String,
}

#[derive(Debug)]
pub struct InitializationBlocked {
    pub keys: Vec<String>,
}

impl fmt::Display for InitializationBlocked {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Instruction initialization blocked by unavailable sources: {}", self.keys.join(", "))
    }
}

impl std::error::Error for InitializationBlocked {}

#[derive(Debug)]
pub struct DuplicateKeyError {
    pub key: String,
}

impl fmt::Display for DuplicateKeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Duplicate instruction key: {}", self.key)
    }
}

impl std::error::Error for DuplicateKeyError {}

pub fn canonical_json(value: &Value) -> String {
    match value {
        Value::Array(items) => {
            format!("[{}]", items.iter().map(canonical_json).collect::<Vec<_>>().join(","))
        }
        Value::Object(entries) => {
            let mut sorted = entries.iter().collect::<Vec<_>>();
            sorted.sort_by(|(left, _), (right, _)| left.encode_utf16().cmp(right.encode_utf16()));
            let members = sorted
                .iter()
                .map(|(key, entry)| format!("{}:{}", encode_json(&key.to_string()), canonical_json(entry)))
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{members}}}")
        }
        _ => encode_json(value),
    }
}

pub fn hash(value: &Value) -> String {
    sha256_hex(canonical_json(value).as_bytes())
}

pub fn read(list: &[Source]) -> Vec<ReadResult> {
    list.iter().map(|source| ReadResult { key: source.key.clone(), value: (source.read)() }).collect()
}

pub fn diff(
    observed: &[ReadResult],
    previous: Option<&[(String, String)]>,
) -> Result<Admission, InitializationBlocked> {
    if previous.is_none() {
        let blocked = observed
            .iter()
            .filter(|entry| matches!(entry.value, Observed::Unavailable))
            .map(|entry| entry.key.clone())
            .collect::<Vec<_>>();
        if !blocked.is_empty() {
            return Err(InitializationBlocked { keys: blocked });
        }
    }
    let mut delta = Vec::new();
    let mut blobs: Vec<(String, Value)> = Vec::new();
    for entry in observed {
        match &entry.value {
            Observed::Unavailable => {}
            Observed::Removed => {
                let stored = previous.is_some_and(|values| values.iter().any(|(key, _)| key == &entry.key));
                if stored {
                    delta.push((entry.key.clone(), None));
                }
            }
            Observed::Value(value) => {
                let next = hash(value);
                let unchanged = previous
                    .is_some_and(|values| values.iter().any(|(key, stored)| key == &entry.key && stored == &next));
                if unchanged {
                    continue;
                }
                delta.push((entry.key.clone(), Some(next.clone())));
                if !blobs.iter().any(|(existing, _)| existing == &next) {
                    blobs.push((next, value.clone()));
                }
            }
        }
    }
    Ok(Admission { delta, blobs })
}

pub fn apply_hash_delta(values: &[(String, String)], delta: &[(String, Option<String>)]) -> Vec<(String, String)> {
    let mut result = values.to_vec();
    for (key, value) in delta {
        match value {
            Some(value) => match result.iter_mut().find(|(existing, _)| existing == key) {
                Some((_, current)) => *current = value.clone(),
                None => result.push((key.clone(), value.clone())),
            },
            None => result.retain(|(existing, _)| existing != key),
        }
    }
    result
}

pub fn combine(lists: Vec<List>) -> Result<List, DuplicateKeyError> {
    let mut sources: List = Vec::new();
    for list in lists {
        for source in list {
            if sources.iter().any(|existing| existing.key == source.key) {
                return Err(DuplicateKeyError { key: source.key });
            }
            sources.push(source);
        }
    }
    Ok(sources)
}

pub fn join(rendered: &[Rendered]) -> String {
    rendered.iter().map(|part| part.text.as_str()).collect::<Vec<_>>().join("\n\n")
}

pub fn render_initial(list: &[Source], values: &[(String, Value)]) -> Vec<Rendered> {
    list.iter()
        .filter_map(|source| {
            let value = values.iter().find(|(key, _)| key == &source.key).map(|(_, value)| value)?;
            let text = (source.initial)(value)?;
            Some(Rendered { key: source.key.clone(), text })
        })
        .collect()
}

pub fn render_update(list: &[Source], previous: &[(String, Value)], delta: &[(String, Option<Value>)]) -> String {
    let rendered = list
        .iter()
        .flat_map(|source| {
            let (_, current) = delta.iter().find(|(key, _)| key == &source.key)?;
            match current {
                None => {
                    let (_, before) = previous.iter().find(|(key, _)| key == &source.key)?;
                    let text = (source.removed.as_ref()?)(before)?;
                    Some(Rendered { key: source.key.clone(), text })
                }
                Some(next) => {
                    let text = match previous.iter().find(|(key, _)| key == &source.key) {
                        Some((_, before)) => (source.changed)(before, next),
                        None => (source.initial)(next),
                    }?;
                    Some(Rendered { key: source.key.clone(), text })
                }
            }
        })
        .collect::<Vec<_>>();
    join(&rendered)
}

pub fn text_source(
    key: &str,
    read: impl Fn() -> Observed + Send + Sync + 'static,
    initial: impl Fn(&str) -> String + Send + Sync + 'static,
    changed: impl Fn(&str, &str) -> Option<String> + Send + Sync + 'static,
) -> Source {
    Source {
        key: key.to_string(),
        read: Box::new(read),
        initial: Box::new(move |value: &Value| {
            let rendered = initial(value.as_str()?);
            (!rendered.is_empty()).then_some(rendered)
        }),
        changed: Box::new(move |previous: &Value, current: &Value| changed(previous.as_str()?, current.as_str()?)),
        removed: None,
    }
}

#[derive(Clone)]
pub struct BuiltinsInput {
    pub directory: String,
    pub workspace_root: String,
    pub vcs: Option<String>,
    pub tmp: String,
}

pub fn builtins(input: BuiltinsInput) -> std::io::Result<List> {
    builtins_with_clock(input, now_ms)
}

pub fn builtins_at(input: BuiltinsInput, now: i64) -> std::io::Result<List> {
    builtins_with_clock(input, move || now)
}

fn builtins_with_clock(input: BuiltinsInput, clock: impl Fn() -> i64 + Send + Sync + 'static) -> std::io::Result<List> {
    std::fs::create_dir_all(&input.tmp)?;
    let date = date_source(clock);
    let environment = environment_source(&input);
    Ok(combine(vec![vec![date], vec![environment]]).expect("builtin instruction keys are distinct"))
}

fn date_source(clock: impl Fn() -> i64 + Send + Sync + 'static) -> Source {
    text_source(
        "core/date",
        move || match Zone::Local.at(clock()) {
            Some(date) => Observed::Value(Value::String(date.date_string())),
            None => Observed::Unavailable,
        },
        |date| format!("Today's date: {date}"),
        |_previous, date| Some(format!("Today's date is now: {date}")),
    )
}

fn environment_source(input: &BuiltinsInput) -> Source {
    let directory = input.directory.clone();
    let workspace_root = input.workspace_root.clone();
    let git = input.vcs.as_deref() == Some("git");
    let tmp = input.tmp.clone();
    text_source(
        "core/environment",
        move || Observed::Value(Value::String(environment(&directory, &workspace_root, git, &tmp))),
        |environment| {
            format!("Here is some useful information about the environment you are running in:\n{environment}")
        },
        |_previous, environment| Some(format!("The environment you are running in is now:\n{environment}")),
    )
}

fn environment(directory: &str, workspace_root: &str, git: bool, tmp: &str) -> String {
    [
        "<env>".to_string(),
        format!("  Working directory: {directory}"),
        format!("  Workspace root folder: {workspace_root}"),
        format!("  Is directory a git repo: {}", if git { "yes" } else { "no" }),
        format!("  Platform: {}", platform()),
        format!(
            "  Prefer {tmp} over generic system temporary directories such as /tmp; it is pre-created and approved \
             for external access."
        ),
        "</env>".to_string(),
    ]
    .join("\n")
}

fn platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}
