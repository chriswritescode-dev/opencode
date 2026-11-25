use std::collections::BTreeSet;

use opencode_config::flag::{FlagSource, FlagState, build_channel, resolve};
use opencode_config::generated::flags::{OPT_IN_ONLY, REGISTRY};
use opencode_contract::codec::decode_json;
use opencode_contract::generated::ConfigEntryDocumentInfo;

use crate::support::area;

#[test]
fn every_flag_case_matches_the_pinned_typescript() {
    let cases = area("flags");
    assert_eq!(cases.len(), 26, "expected 26 flag cases, found {}", cases.len());
    for case in &cases {
        let decoded: Vec<ConfigEntryDocumentInfo> = case.value["entries"]
            .as_array()
            .unwrap_or_else(|| panic!("{} entries is not an array", case.name))
            .iter()
            .map(|entry| {
                assert_eq!(entry["type"], "document", "{} entry type", case.name);
                decode_json::<ConfigEntryDocumentInfo>(&entry["info"].to_string())
                    .unwrap_or_else(|error| panic!("{} entry info failed to decode: {error}", case.name))
            })
            .collect();
        let refs: Vec<&_> = decoded.iter().collect();
        let channel = case.value["channel"].as_str().unwrap();
        let overrides = case.value["overrides"].as_str();
        let resolved = resolve(&refs, channel, overrides);
        let keys: Vec<&str> = resolved.iter().map(|(key, _)| *key).collect();
        let registry: Vec<&str> = REGISTRY.iter().map(|(key, _)| *key).collect();
        assert_eq!(keys, registry, "{} resolved keys", case.name);

        let expected = case
            .value
            .get("expected")
            .and_then(serde_json::Value::as_object)
            .unwrap_or_else(|| panic!("{} expected is not an object", case.name));
        let expected_keys: BTreeSet<&str> = expected.keys().map(String::as_str).collect();
        let registry_keys: BTreeSet<&str> = registry.iter().copied().collect();
        assert_eq!(expected_keys, registry_keys, "{} expected keys", case.name);

        for (key, state) in &resolved {
            let entry = &expected[*key];
            let source = match entry["source"].as_str() {
                Some("env") => FlagSource::Env,
                Some("config") => FlagSource::Config,
                Some("default") => FlagSource::Default,
                other => panic!("{} key {key} has unknown source {other:?}", case.name),
            };
            let enabled =
                entry["enabled"].as_bool().unwrap_or_else(|| panic!("{} key {key} enabled is not a bool", case.name));
            assert_eq!(*state, FlagState { enabled, source }, "{} key {key}", case.name);
        }
    }
}

#[test]
fn the_corpus_covers_the_pinned_registry() {
    let keys: BTreeSet<&str> = REGISTRY.iter().map(|(key, _)| *key).collect();
    for key in ["model_speeds", "mcp_skills", "opencode_api"] {
        assert!(keys.contains(key), "registry is missing {key}");
    }
    for key in OPT_IN_ONLY {
        assert!(keys.contains(key), "opt-in key {key} is not a registry key");
    }
}

#[test]
fn build_channel_defaults_to_local() {
    assert_eq!(build_channel(), option_env!("OPENCODE_CHANNEL").unwrap_or("local"));
}
