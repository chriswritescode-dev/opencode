use opencode_api_types::js::decode_component;
use opencode_contract::codec::DecodeError;
use opencode_contract::generated::operations::REQUEST_SLOTS;
use opencode_contract::request::{RawRequest, location_directory, parse_query, wildcard};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const REQUESTS_JSON: &str = include_str!("../corpus/requests.json");

type SlotDecode = fn(&RawRequest) -> Result<String, DecodeError>;

fn fixture() -> Value {
    serde_json::from_str(REQUESTS_JSON).unwrap()
}

fn roots() -> BTreeMap<(&'static str, &'static str), SlotDecode> {
    REQUEST_SLOTS.iter().map(|slot| ((slot.operation_id, slot.slot), slot.decode)).collect()
}

fn plain() -> RawRequest {
    RawRequest { path_params: Vec::new(), query: String::new(), headers: Vec::new(), body: Vec::new() }
}

fn request(slot: &str, raw: &Value) -> RawRequest {
    let mut request = plain();
    match slot {
        "params" => {
            request.path_params = raw["params"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(key, value)| {
                    let encoded = value.as_str().unwrap();
                    (key.clone(), decode_component(encoded).unwrap_or_else(|| encoded.to_owned()))
                })
                .collect();
        }
        "query" => request.query = raw["query"].as_str().unwrap().to_owned(),
        "headers" => {
            request.headers = raw["headers"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(key, value)| (key.to_ascii_lowercase(), value.as_str().unwrap().to_owned()))
                .collect();
        }
        "body" => request.body = raw["body"].as_str().unwrap().as_bytes().to_vec(),
        other => panic!("unknown request slot {other}"),
    }
    request
}

#[test]
fn the_corpus_and_the_request_roots_match() {
    let fixture = fixture();
    let roots = roots();
    let mut expected: BTreeSet<(&str, &str)> = BTreeSet::new();
    for (operation, slots) in fixture.as_object().unwrap() {
        for slot in slots.as_object().unwrap().keys() {
            expected.insert((operation.as_str(), slot.as_str()));
        }
    }
    let actual: BTreeSet<(&str, &str)> = roots.keys().copied().collect();
    assert_eq!(
        actual,
        expected,
        "request roots differ: corpus only {:?}, rust only {:?}",
        expected.difference(&actual).collect::<Vec<_>>(),
        actual.difference(&expected).collect::<Vec<_>>()
    );
}

#[test]
fn every_valid_request_decodes_to_expected() {
    let fixture = fixture();
    let roots = roots();
    for (operation, slots) in fixture.as_object().unwrap() {
        for (slot, entry) in slots.as_object().unwrap() {
            let decode = roots
                .get(&(operation.as_str(), slot.as_str()))
                .unwrap_or_else(|| panic!("no request root for {operation} {slot}"));
            let valid = entry["valid"].as_array().unwrap();
            assert!(!valid.is_empty(), "{operation} {slot} has no valid samples");
            for sample in valid {
                let raw = request(slot, &sample["raw"]);
                let expected = sample["expected"].as_str().unwrap();
                let actual =
                    decode(&raw).unwrap_or_else(|error| panic!("{operation} {slot} failed to decode: {error}"));
                assert_eq!(actual, expected, "{operation} {slot} raw {}", sample["raw"]);
            }
        }
    }
}

#[test]
fn every_invalid_request_is_rejected() {
    let fixture = fixture();
    let roots = roots();
    let mut rejected = 0;
    for (operation, slots) in fixture.as_object().unwrap() {
        for (slot, entry) in slots.as_object().unwrap() {
            let decode = roots
                .get(&(operation.as_str(), slot.as_str()))
                .unwrap_or_else(|| panic!("no request root for {operation} {slot}"));
            for sample in entry["invalid"].as_array().unwrap() {
                let raw = request(slot, sample);
                rejected += 1;
                assert!(
                    decode(&raw).is_err(),
                    "{operation} {slot} accepted invalid request {sample} as {:?}",
                    decode(&raw)
                );
            }
        }
    }
    assert!(rejected > 0, "the request corpus has no invalid samples");
}

fn query_request(query: &str) -> RawRequest {
    RawRequest { query: query.to_owned(), ..plain() }
}

fn header_request(headers: &[(&str, &str)]) -> RawRequest {
    RawRequest {
        headers: headers.iter().map(|(key, value)| ((*key).to_owned(), (*value).to_owned())).collect(),
        ..plain()
    }
}

#[test]
fn the_location_directory_matches_request_ref() {
    assert_eq!(location_directory(&query_request("location[directory]=%2Ftmp%2Fa")), Some("/tmp/a".to_owned()));
    assert_eq!(
        location_directory(&header_request(&[("x-opencode-directory", "%2Ftmp%2Fb")])),
        Some("/tmp/b".to_owned())
    );
    let both = RawRequest {
        query: "location[directory]=%2Ftmp%2Fa".to_owned(),
        headers: vec![("x-opencode-directory".to_owned(), "%2Ftmp%2Fb".to_owned())],
        ..plain()
    };
    assert_eq!(location_directory(&both), Some("/tmp/a".to_owned()));
    let empty = RawRequest {
        query: "location[directory]=".to_owned(),
        headers: vec![("x-opencode-directory".to_owned(), "%2Ftmp%2Fb".to_owned())],
        ..plain()
    };
    assert_eq!(location_directory(&empty), Some("/tmp/b".to_owned()));
    let none = RawRequest {
        query: "location[directory]=".to_owned(),
        headers: vec![("x-opencode-directory".to_owned(), String::new())],
        ..plain()
    };
    assert_eq!(location_directory(&none), None);
    assert_eq!(
        location_directory(&header_request(&[("x-opencode-directory", "%2Ftmp%2Fb+c")])),
        Some("/tmp/b+c".to_owned())
    );
    assert_eq!(location_directory(&header_request(&[("x-opencode-directory", "%2Z")])), Some("%2Z".to_owned()));
    assert_eq!(location_directory(&query_request("location[directory]=%2Ftmp%2Fa%ZZ")), Some("/tmp/a%ZZ".to_owned()));
    assert_eq!(location_directory(&query_request("location[directory]=%2Ftmp%FF")), Some("/tmp\u{FFFD}".to_owned()));
    assert_eq!(location_directory(&query_request("location%5Bdirectory%5D=%2Fx")), Some("/x".to_owned()));
}

#[test]
fn the_query_parser_matches_fast_querystring() {
    assert_eq!(
        parse_query("a=1&a=2&b=%2Fx&c&d=&plus=a+b"),
        vec![
            ("a".to_owned(), vec!["1".to_owned(), "2".to_owned()]),
            ("b".to_owned(), vec!["/x".to_owned()]),
            ("c".to_owned(), vec![String::new()]),
            ("d".to_owned(), vec![String::new()]),
            ("plus".to_owned(), vec!["a b".to_owned()]),
        ]
    );
    assert_eq!(
        parse_query("location[directory]=%2Ftmp%2Fa"),
        vec![("location[directory]".to_owned(), vec!["/tmp/a".to_owned()])]
    );
    assert_eq!(parse_query("&&=&="), vec![(String::new(), vec![String::new(), String::new()])]);
    assert_eq!(parse_query("bad=%2Z"), vec![("bad".to_owned(), vec!["%2Z".to_owned()])]);
}

#[test]
fn the_wildcard_tail_is_exposed() {
    let request = RawRequest {
        path_params: vec![("artifactID".to_owned(), "art_1".to_owned()), ("*".to_owned(), "a/b".to_owned())],
        ..plain()
    };
    assert_eq!(wildcard(&request), Some("a/b"));
    assert_eq!(wildcard(&plain()), None);
}
