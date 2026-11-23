use opencode_contract::codec::{decode_json, encode_json};
use opencode_contract::generated::{
    DURABLE_EVENTS, DurableEvent, PUBLIC_EVENT_TYPES, PublicEvent, SessionLogItem, corpus_roots,
};
use serde_json::Value;
use std::collections::BTreeSet;

const EVENTS_JSON: &str = include_str!("../corpus/events.json");
const CORPUS_JSON: &str = include_str!("../corpus/schemas.json");

fn events() -> Value {
    serde_json::from_str(EVENTS_JSON).unwrap()
}

fn corpus() -> Value {
    serde_json::from_str(CORPUS_JSON).unwrap()
}

fn sample<'a>(corpus: &'a Value, root: &str) -> (&'a str, &'a str) {
    let valid = corpus[root]["valid"].as_array().unwrap_or_else(|| panic!("corpus root {root} has no valid samples"));
    let first = valid.first().unwrap_or_else(|| panic!("corpus root {root} has no valid samples"));
    (first["input"].as_str().unwrap(), first["expected"].as_str().unwrap())
}

#[test]
fn the_public_event_types_match_the_pinned_list() {
    let fixture = events();
    let expected: Vec<&str> =
        fixture["public"].as_array().unwrap().iter().map(|value| value.as_str().unwrap()).collect();

    assert_eq!(PUBLIC_EVENT_TYPES.len(), 101, "expected 101 public event types, found {}", PUBLIC_EVENT_TYPES.len());
    assert_eq!(PUBLIC_EVENT_TYPES.as_slice(), expected.as_slice(), "public event types differ from the pin");

    let unique: BTreeSet<&str> = PUBLIC_EVENT_TYPES.iter().copied().collect();
    assert_eq!(unique.len(), PUBLIC_EVENT_TYPES.len(), "duplicate public event types");
}

#[test]
fn the_durable_events_match_the_pinned_list() {
    let fixture = events();
    let expected: Vec<(String, u32, String)> = fixture["durable"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            let triple = entry.as_array().unwrap();
            (
                triple[0].as_str().unwrap().to_owned(),
                triple[1].as_u64().unwrap() as u32,
                triple[2].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let actual: Vec<(String, u32, String)> = DURABLE_EVENTS
        .iter()
        .map(|(tag, version, aggregate)| ((*tag).to_owned(), *version, (*aggregate).to_owned()))
        .collect();

    assert_eq!(DURABLE_EVENTS.len(), 49, "expected 49 durable events, found {}", DURABLE_EVENTS.len());
    assert_eq!(actual, expected, "durable events differ from the pin");

    let unique: BTreeSet<(&str, u32)> = DURABLE_EVENTS.iter().map(|(tag, version, _)| (*tag, *version)).collect();
    assert_eq!(unique.len(), DURABLE_EVENTS.len(), "duplicate durable type and version pairs");
    for (tag, version, aggregate) in DURABLE_EVENTS {
        assert!(version >= 1, "durable event {tag} has version {version}");
        assert!(!aggregate.is_empty(), "durable event {tag} has no aggregate");
    }
}

#[test]
fn every_public_type_and_durable_entry_has_a_corpus_root() {
    let roots: BTreeSet<&str> = corpus_roots().iter().map(|(root, _)| *root).collect();
    assert!(roots.contains("SessionLogItem"), "the session log item has no corpus root");
    for tag in PUBLIC_EVENT_TYPES {
        assert!(roots.contains(tag), "public event {tag} has no corpus root");
    }
    for (tag, version, _) in DURABLE_EVENTS {
        let key = format!("{tag}.{version}");
        assert!(roots.contains(key.as_str()), "durable event {key} has no corpus root");
    }
    let fixture = events();
    for tag in fixture["sessionLog"].as_array().unwrap() {
        let tag = tag.as_str().unwrap();
        if tag == "log.synced" {
            continue;
        }
        let version = DURABLE_EVENTS
            .iter()
            .find(|(name, _, _)| *name == tag)
            .map(|(_, version, _)| *version)
            .unwrap_or_else(|| panic!("session log type {tag} is not in the durable manifest"));
        let key = format!("{tag}.{version}");
        assert!(roots.contains(key.as_str()), "session log type {tag} has no corpus root {key}");
    }
}

#[test]
fn every_public_event_decodes_through_the_enum() {
    let corpus = corpus();
    for tag in PUBLIC_EVENT_TYPES {
        let (input, expected) = sample(&corpus, tag);
        let event: PublicEvent =
            decode_json(input).unwrap_or_else(|error| panic!("public event {tag} failed to decode: {error}"));
        assert_eq!(encode_json(&event), expected, "public event {tag} does not round trip");
    }
}

#[test]
fn every_durable_event_decodes_through_the_enum() {
    let corpus = corpus();
    for (tag, version, _) in DURABLE_EVENTS {
        let key = format!("{tag}.{version}");
        let (input, expected) = sample(&corpus, &key);
        let event: DurableEvent =
            decode_json(input).unwrap_or_else(|error| panic!("durable event {key} failed to decode: {error}"));
        assert_eq!(encode_json(&event), expected, "durable event {key} does not round trip");
    }
}

#[test]
fn an_unknown_event_type_is_rejected() {
    let result = decode_json::<PublicEvent>(r#"{"id":"evt_","created":0,"type":"not.an.event","data":{}}"#);
    assert!(result.is_err(), "expected an unknown public event to be rejected, got {result:?}");
}

#[test]
fn an_explicit_null_helper_optional_field_is_rejected() {
    let corpus = corpus();
    let (input, _) = sample(&corpus, "session.created");
    let mut value: Value = serde_json::from_str(input).unwrap();
    value["data"]["parentID"] = Value::Null;

    let result = decode_json::<PublicEvent>(&value.to_string());
    assert!(result.is_err(), "expected the null helper optional to be rejected, got {result:?}");

    value["data"].as_object_mut().unwrap().remove("parentID");
    let event: PublicEvent =
        decode_json(&value.to_string()).unwrap_or_else(|error| panic!("absent optional failed: {error}"));
    let encoded = encode_json(&event);
    assert!(!encoded.contains("parentID"), "expected the absent optional to stay absent, got {encoded}");
}

#[test]
fn an_explicit_null_undefined_optional_field_is_preserved() {
    let input = r#"{"id":"evt_1","metadata":null,"type":"server.connected","data":{}}"#;
    let event: PublicEvent =
        decode_json(input).unwrap_or_else(|error| panic!("null undefined optional failed: {error}"));
    let encoded = encode_json(&event);
    assert!(encoded.contains(r#""metadata":null"#), "expected the null undefined optional to stay null, got {encoded}");

    let absent = r#"{"id":"evt_1","type":"server.connected","data":{}}"#;
    let event: PublicEvent =
        decode_json(absent).unwrap_or_else(|error| panic!("absent undefined optional failed: {error}"));
    let encoded = encode_json(&event);
    assert!(!encoded.contains("metadata"), "expected the absent undefined optional to stay absent, got {encoded}");
}

#[test]
fn an_unknown_durable_type_or_version_is_rejected() {
    let corpus = corpus();
    let (input, _) = sample(&corpus, "session.created.1");

    let mut unknown: Value = serde_json::from_str(input).unwrap();
    unknown["type"] = Value::from("not.a.durable.event");
    let result = decode_json::<DurableEvent>(&unknown.to_string());
    assert!(result.is_err(), "expected an unknown durable event to be rejected, got {result:?}");

    let mut version: Value = serde_json::from_str(input).unwrap();
    version["durable"]["version"] = Value::from(2);
    let result = decode_json::<DurableEvent>(&version.to_string());
    assert!(result.is_err(), "expected an unknown durable version to be rejected, got {result:?}");
}

#[test]
fn the_session_log_item_holds_exactly_the_session_log_types() {
    let corpus = corpus();
    let fixture = events();
    for tag in fixture["sessionLog"].as_array().unwrap() {
        let tag = tag.as_str().unwrap();
        if tag == "log.synced" {
            let item = decode_json::<SessionLogItem>(r#"{"type":"log.synced","aggregateID":"x"}"#);
            assert!(item.is_ok(), "expected the log marker to decode, got {item:?}");
            continue;
        }
        let version = DURABLE_EVENTS
            .iter()
            .find(|(name, _, _)| *name == tag)
            .map(|(_, version, _)| *version)
            .unwrap_or_else(|| panic!("session log type {tag} is not in the durable manifest"));
        let key = format!("{tag}.{version}");
        let (input, _) = sample(&corpus, &key);
        let item = decode_json::<SessionLogItem>(input);
        assert!(item.is_ok(), "expected session log type {tag} to decode, got {item:?}");
    }

    let (worktree, _) = sample(&corpus, "worktree.resolved.1");
    let item = decode_json::<SessionLogItem>(worktree);
    assert!(item.is_err(), "expected worktree.resolved to stay out of the session log, got {item:?}");
}
