pub mod builtins;
pub mod discovery;

use std::collections::HashSet;
use std::sync::{Arc, LazyLock};

use opencode_api_types::js::number_to_string;

pub const KEY_PATTERN: &str = r"^[a-z0-9][a-z0-9._-]*/[a-z0-9][a-z0-9._/-]*$";

static PATTERN: LazyLock<regex_lite::Regex> =
    LazyLock::new(|| regex_lite::Regex::new(KEY_PATTERN).expect("the instruction key pattern compiles"));

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key(String);

impl Key {
    pub fn new(value: impl Into<String>) -> Result<Key, InvalidKey> {
        let value = value.into();
        if !PATTERN.is_match(&value) {
            return Err(InvalidKey(value));
        }
        Ok(Key(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("invalid instruction key: {0}")]
pub struct InvalidKey(pub String);

#[derive(Debug, thiserror::Error)]
#[error("duplicate instruction key: {}", .key.as_str())]
pub struct DuplicateKeyError {
    pub key: Key,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Observed {
    Value(serde_json::Value),
    Removed,
    Unavailable,
}

pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

pub trait Source: Send + Sync {
    fn key(&self) -> &Key;
    fn read<'a>(&'a self) -> BoxFuture<'a, Observed>;
    fn initial(&self, value: &serde_json::Value) -> Option<String>;
}

pub type List = Vec<Arc<dyn Source>>;

type Render = Box<dyn Fn(&serde_json::Value) -> Option<String> + Send + Sync>;

pub struct FixedSource {
    key: Key,
    observed: Observed,
    render: Render,
}

impl FixedSource {
    pub fn new(
        key: Key,
        observed: Observed,
        render: impl Fn(&serde_json::Value) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        FixedSource { key, observed, render: Box::new(render) }
    }
}

impl Source for FixedSource {
    fn key(&self) -> &Key {
        &self.key
    }

    fn read<'a>(&'a self) -> BoxFuture<'a, Observed> {
        let observed = self.observed.clone();
        Box::pin(async move { observed })
    }

    fn initial(&self, value: &serde_json::Value) -> Option<String> {
        (self.render)(value)
    }
}

pub fn fixed(
    key: Key,
    value: serde_json::Value,
    render: impl Fn(&serde_json::Value) -> Option<String> + Send + Sync + 'static,
) -> Arc<dyn Source> {
    Arc::new(FixedSource::new(key, Observed::Value(value), render))
}

pub struct Rendered {
    pub key: Key,
    pub text: String,
}

pub fn canonical_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", parts.join(","))
        }
        serde_json::Value::Object(object) => {
            let mut entries: Vec<(&String, &serde_json::Value)> = object.iter().collect();
            entries.sort_by(|(a, _), (b, _)| a.encode_utf16().cmp(b.encode_utf16()));
            let parts: Vec<String> = entries
                .into_iter()
                .map(|(key, value)| {
                    let key = serde_json::to_string(key).expect("a JSON object key serializes");
                    format!("{key}:{}", canonical_json(value))
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        serde_json::Value::Number(number) => number.as_f64().map_or_else(|| number.to_string(), number_to_string),
        value => serde_json::to_string(value).expect("a JSON value serializes"),
    }
}

pub fn hash(value: &serde_json::Value) -> String {
    ring::digest::digest(&ring::digest::SHA256, canonical_json(value).as_bytes())
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn combine(values: impl IntoIterator<Item = List>) -> Result<List, DuplicateKeyError> {
    let sources: List = values.into_iter().flatten().collect();
    let mut seen = HashSet::new();
    for source in &sources {
        if !seen.insert(source.key().clone()) {
            return Err(DuplicateKeyError { key: source.key().clone() });
        }
    }
    Ok(sources)
}

pub fn render_initial(value: &[Arc<dyn Source>], values: &serde_json::Map<String, serde_json::Value>) -> Vec<Rendered> {
    value
        .iter()
        .flat_map(|source| {
            let value = values.get(source.key().as_str())?;
            let text = source.initial(value)?;
            Some(Rendered { key: source.key().clone(), text })
        })
        .collect()
}

pub fn is_unavailable(observed: &Observed) -> bool {
    matches!(observed, Observed::Unavailable)
}

pub fn is_removed(observed: &Observed) -> bool {
    matches!(observed, Observed::Removed)
}
