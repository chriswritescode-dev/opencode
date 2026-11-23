use std::collections::HashMap;

use opencode_contract::codec::encode_json;
use opencode_contract::generated::ConfigEntryDocumentInfo;

use crate::generated::flags::{OPT_IN_ONLY, REGISTRY, WILDCARD};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlagSource {
    Env,
    Config,
    Default,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlagState {
    pub enabled: bool,
    pub source: FlagSource,
}

pub fn resolve(
    entries: &[&ConfigEntryDocumentInfo],
    channel: &str,
    overrides: Option<&str>,
) -> Vec<(&'static str, FlagState)> {
    let env = parse_overrides(overrides.unwrap_or_default());
    let all = env.get(WILDCARD) == Some(&true);
    let experimental = entries.iter().rev().find_map(|entry| entry.experimental.as_ref());
    let settings = experimental.map(|experimental| {
        serde_json::from_str::<serde_json::Value>(&encode_json(experimental))
            .expect("the generated experimental settings encode to a JSON object")
    });
    let all_flags = entries
        .iter()
        .rev()
        .find_map(|entry| entry.experimental.as_ref().and_then(|experimental| experimental.all_flags))
        == Some(true);
    REGISTRY
        .iter()
        .map(|(key, _)| {
            let opt_in_only = OPT_IN_ONLY.contains(key);
            let override_value = env.get(*key).copied().or((all && !opt_in_only).then_some(true));
            let value = settings.as_ref().and_then(|settings| settings.get(*key)).and_then(configured_value);
            let configured = value.or((all_flags && !opt_in_only).then_some(true));
            let (enabled, source) = match (override_value, configured) {
                (Some(value), _) => (value, FlagSource::Env),
                (None, Some(value)) => (value, FlagSource::Config),
                (None, None) => (!opt_in_only && on_by_default(channel), FlagSource::Default),
            };
            (*key, FlagState { enabled, source })
        })
        .collect()
}

pub fn build_channel() -> &'static str {
    option_env!("OPENCODE_CHANNEL").unwrap_or("local")
}

pub fn overrides_from_env() -> Option<String> {
    std::env::var("OPENCODE_FLAGS").ok()
}

fn parse_overrides(overrides: &str) -> HashMap<&str, bool> {
    overrides
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| match item.strip_prefix('-') {
            Some(key) => (key, false),
            None => (item, true),
        })
        .collect()
}

fn configured_value(value: &serde_json::Value) -> Option<bool> {
    match value {
        serde_json::Value::Bool(value) => Some(*value),
        serde_json::Value::Object(_) => Some(true),
        _ => None,
    }
}

fn on_by_default(channel: &str) -> bool {
    channel == "local" || channel == "dev"
}
