use opencode_contract::codec::DecodeError;
use opencode_contract::generated::corpus_roots;
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

    assert_eq!(corpus_roots().len(), 339, "expected 339 rust corpus roots, found {}", corpus_roots().len());
    assert_eq!(fixture_roots.len(), 339, "expected 339 fixture roots, found {}", fixture_roots.len());
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
