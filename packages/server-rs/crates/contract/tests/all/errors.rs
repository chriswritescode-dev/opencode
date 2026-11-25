use opencode_contract::codec::{decode_json, encode_json};
use opencode_contract::generated::{ApiError, ERROR_STATUS};
use serde_json::Value;
use std::collections::BTreeSet;

const CORPUS_JSON: &str = include_str!("../corpus/schemas.json");

fn corpus() -> Value {
    serde_json::from_str(CORPUS_JSON).unwrap()
}

fn sample_input<'a>(corpus: &'a Value, tag: &str) -> &'a str {
    corpus[tag]["valid"][0]["input"].as_str().unwrap_or_else(|| panic!("corpus root {tag} has no valid sample"))
}

#[test]
fn the_error_status_table_has_the_expected_shape() {
    assert_eq!(ERROR_STATUS.len(), 46, "expected 46 error statuses, found {}", ERROR_STATUS.len());

    let tags: BTreeSet<&str> = ERROR_STATUS.iter().map(|(tag, _)| *tag).collect();
    assert_eq!(tags.len(), 46, "expected 46 unique error tags, found {}", tags.len());

    let mut sorted: Vec<(&str, u16)> = ERROR_STATUS.to_vec();
    sorted.sort();
    assert_eq!(ERROR_STATUS.as_slice(), sorted.as_slice(), "ERROR_STATUS is not sorted by tag");
}

#[test]
fn every_error_tag_decodes_with_its_status_and_round_trips() {
    let corpus = corpus();
    for (tag, status) in ERROR_STATUS {
        let input = sample_input(&corpus, tag);
        let error: ApiError =
            decode_json(input).unwrap_or_else(|error| panic!("root {tag} input {input} failed to decode: {error}"));
        assert_eq!(error.tag(), tag, "wrong tag for root {tag} input {input}");
        assert_eq!(error.status(), status, "wrong status for root {tag} input {input}");

        let encoded = encode_json(&error);
        let tag_json = serde_json::to_string(tag).unwrap();
        assert!(
            encoded.starts_with(&format!("{{\"_tag\":{tag_json}")),
            "root {tag} encoded as {encoded}, expected _tag {tag_json} first"
        );

        let decoded: ApiError = decode_json(&encoded)
            .unwrap_or_else(|error| panic!("root {tag} encoded as {encoded} failed to decode: {error}"));
        assert_eq!(decoded.tag(), tag, "wrong tag after round trip for root {tag}");
        assert_eq!(decoded.status(), status, "wrong status after round trip for root {tag}");
    }
}

#[test]
fn an_unknown_tag_is_rejected() {
    let result = decode_json::<ApiError>("{\"_tag\":\"NotAnError\"}");
    assert!(result.is_err(), "expected unknown tag to be rejected, got {result:?}");
}
