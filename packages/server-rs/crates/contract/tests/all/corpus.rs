use opencode_contract::codec::{DecodeError, Encode, JsonMap, decode_json, round_trip, write_struct_object};
use opencode_contract::generated::corpus_roots;
use opencode_contract::generated::requests::{FormFields, GenerateImageFile};
use opencode_contract::generated::{SessionActiveResponse, V2EventRpc};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const CORPUS_JSON: &str = include_str!("../corpus/schemas.json");

type RoundTrip = fn(&str) -> Result<String, DecodeError>;

fn corpus() -> Value {
    serde_json::from_str(CORPUS_JSON).unwrap()
}

fn rust_roots() -> BTreeMap<&'static str, RoundTrip> {
    corpus_roots().iter().copied().collect()
}

#[test]
fn the_corpus_and_the_rust_roots_match() {
    let fixture = corpus();
    let fixture_roots: BTreeSet<&str> = fixture.as_object().unwrap().keys().map(String::as_str).collect();
    let roots: BTreeSet<&str> = corpus_roots().iter().map(|(root, _)| *root).collect();

    assert_eq!(corpus_roots().len(), 349, "expected 349 rust corpus roots, found {}", corpus_roots().len());
    assert_eq!(fixture_roots.len(), 349, "expected 349 fixture roots, found {}", fixture_roots.len());
    assert_eq!(
        roots,
        fixture_roots,
        "corpus roots differ: fixture only {:?}, rust only {:?}",
        fixture_roots.difference(&roots).collect::<Vec<_>>(),
        roots.difference(&fixture_roots).collect::<Vec<_>>()
    );
}

#[test]
fn every_root_has_valid_samples() {
    let corpus = corpus();
    let mut invalid = 0;
    for (root, entry) in corpus.as_object().unwrap() {
        let valid = entry["valid"].as_array().unwrap();
        assert!(!valid.is_empty(), "corpus root {root} has no valid samples");
        invalid += entry["invalid"].as_array().unwrap().len();
    }
    assert!(invalid > 0, "corpus has no invalid samples");
}

#[test]
fn every_valid_corpus_sample_round_trips() {
    let corpus = corpus();
    let roots = rust_roots();
    for (root, entry) in corpus.as_object().unwrap() {
        let round_trip = *roots.get(root.as_str()).unwrap_or_else(|| panic!("no rust root for corpus root {root}"));
        for sample in entry["valid"].as_array().unwrap() {
            let input = sample["input"].as_str().unwrap();
            let expected = sample["expected"].as_str().unwrap();
            let actual =
                round_trip(input).unwrap_or_else(|error| panic!("root {root} input {input} failed to decode: {error}"));
            assert_eq!(actual, expected, "root {root} input {input}");
        }
    }
}

#[test]
fn every_invalid_corpus_sample_is_rejected() {
    let corpus = corpus();
    let roots = rust_roots();
    for (root, entry) in corpus.as_object().unwrap() {
        let round_trip = *roots.get(root.as_str()).unwrap_or_else(|| panic!("no rust root for corpus root {root}"));
        for input in entry["invalid"].as_array().unwrap() {
            let input = input.as_str().unwrap();
            let result = round_trip(input);
            assert!(result.is_err(), "root {root} accepted invalid input {input} as {result:?}");
        }
    }
}

#[test]
fn objects_encode_index_keys_first_in_numeric_order() {
    for (input, expected) in [
        (r#"{"b":1,"10":2,"a":3,"2":4}"#, r#"{"2":4,"10":2,"b":1,"a":3}"#),
        (r#"{"100":1,"20":2,"3":3}"#, r#"{"3":3,"20":2,"100":1}"#),
        (r#"{"b":1,"4294967294":2,"0":3}"#, r#"{"0":3,"4294967294":2,"b":1}"#),
    ] {
        assert_eq!(round_trip::<Value>(input).unwrap(), expected, "value input {input}");
        assert_eq!(round_trip::<JsonMap<f64>>(input).unwrap(), expected, "json map input {input}");
    }
}

#[test]
fn objects_keep_non_index_keys_in_insertion_order() {
    for input in [r#"{"4294967295":1,"01":2,"-1":3,"b":4}"#, r#"{"0.5":5,"1e3":6,"4294967296":7," ":8}"#] {
        assert_eq!(round_trip::<Value>(input).unwrap(), input, "value input {input}");
        assert_eq!(round_trip::<JsonMap<f64>>(input).unwrap(), input, "json map input {input}");
    }
}

#[test]
fn structs_with_rest_encode_declared_index_keys_with_the_rest() {
    let mut out = String::new();
    write_struct_object(
        &mut out,
        &[("0", Some(&1.0f64 as &dyn Encode)), ("timeout", Some(&3.0f64 as &dyn Encode))],
        [("2", Some(&2.0f64 as &dyn Encode)), ("zz", Some(&9.0f64 as &dyn Encode))],
    );
    assert_eq!(out, r#"{"0":1,"2":2,"timeout":3,"zz":9}"#);
}

#[test]
fn non_finite_numbers_encode_as_null() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(opencode_contract::codec::encode_json(&value), "null", "value {value}");
    }
}

#[test]
fn every_generated_pattern_compiles() {
    opencode_contract::generated::compile_all_patterns();
}

#[test]
fn strict_checks_match_the_typescript_schemas() {
    decode_json::<GenerateImageFile>(r#"{"data":"","mime":"IMAGE/png"}"#).expect("IMAGE/png is an image mime");

    assert!(decode_json::<FormFields>("[]").is_err(), "FormFields must not be empty");

    decode_json::<V2EventRpc>(
        r#"{"id":"evt_1","created":1,"type":"rpc.call","location":{"directory":"/tmp"},"data":{}}"#,
    )
    .expect("rpc.call matches rpc.${string}");
    assert!(
        decode_json::<V2EventRpc>(
            r#"{"id":"evt_1","created":1,"type":"foo","location":{"directory":"/tmp"},"data":{}}"#,
        )
        .is_err(),
        "type foo must not match rpc.${{string}}"
    );

    decode_json::<SessionActiveResponse>(r#"{"data":{"ses_1":{"type":"running"}}}"#)
        .expect("a session id key is accepted");
    assert_eq!(
        round_trip::<SessionActiveResponse>(r#"{"data":{"foo":{"type":"running"}}}"#).unwrap(),
        r#"{"data":{}}"#,
        "record keys that do not start with ses are dropped"
    );
}
