use opencode_config::normalize::{Diagnostic, Normalized, normalize};
use opencode_config::parse::parse_info;
use opencode_contract::codec::encode_json;
use serde_json::{Value, json};

use crate::support::{Case, area};

fn diagnostics_value(diagnostics: &[Diagnostic]) -> Value {
    Value::Array(
        diagnostics
            .iter()
            .map(|diagnostic| {
                json!({
                    "kind": diagnostic.kind.as_str(),
                    "path": diagnostic.path,
                    "message": diagnostic.message,
                })
            })
            .collect(),
    )
}

#[test]
fn current_cases_match_the_pinned_typescript() {
    let cases = area("normalize");
    assert_eq!(cases.len(), 131, "expected 131 normalize cases, found {}", cases.len());
    let current = cases.iter().filter(|case| case.value["group"] == "current").count();
    let v1 = cases.iter().filter(|case| case.value["group"] == "v1").count();
    assert_eq!(current, 93, "expected 93 current normalize cases, found {current}");
    assert_eq!(v1, 38, "expected 38 v1 normalize cases, found {v1}");
    for case in &cases {
        let group = case.value["group"].as_str();
        assert!(matches!(group, Some("current" | "v1")), "{} has unknown group {group:?}", case.name);
    }

    verify_group(&cases, "current");
}

#[test]
fn v1_cases_match_the_pinned_typescript() {
    verify_group(&area("normalize"), "v1");
}

fn verify_group(cases: &[Case], group: &str) {
    for case in cases {
        if case.value["group"] != group {
            continue;
        }
        let input = case.value["input"].as_str().unwrap_or_else(|| panic!("{} input is not a string", case.name));
        let parsed = opencode_local::jsonc::parse(input);
        let result = parsed.as_ref().map(normalize);
        let expected_normalized = &case.value["normalized"];

        if expected_normalized.is_null() {
            assert!(parsed.is_none(), "{} jsonc parsed but the corpus expects no normalized result", case.name);
        } else {
            let result = result.as_ref().unwrap_or_else(|| {
                panic!("{} jsonc failed to parse but the corpus expects a normalized result", case.name)
            });
            let expected_type = expected_normalized["type"]
                .as_str()
                .unwrap_or_else(|| panic!("{} normalized type is not a string", case.name));
            match expected_type {
                "normalized" => {
                    let Normalized::Normalized { encoded, diagnostics } = result else {
                        panic!("{} expected a normalized result, got {result:?}", case.name)
                    };
                    let encoded = Value::Object(encoded.clone());
                    assert_eq!(
                        encoded, expected_normalized["encoded"],
                        "{} normalized encoded configuration",
                        case.name
                    );
                    assert_eq!(
                        serde_json::to_string(&encoded).unwrap(),
                        serde_json::to_string(&expected_normalized["encoded"]).unwrap(),
                        "{} normalized encoded key order",
                        case.name
                    );
                    assert_eq!(
                        diagnostics_value(diagnostics),
                        expected_normalized["diagnostics"],
                        "{} normalized diagnostics",
                        case.name
                    );
                }
                "rejected" => {
                    let Normalized::Rejected { diagnostics } = result else {
                        panic!("{} expected a rejected result, got {result:?}", case.name)
                    };
                    assert_eq!(
                        diagnostics_value(diagnostics),
                        expected_normalized["diagnostics"],
                        "{} rejected diagnostics",
                        case.name
                    );
                }
                other => panic!("{} has unknown normalized type {other:?}", case.name),
            }
        }

        let outcome = parse_info(input, &case.name);
        let expected_parsed = &case.value["parsed"];
        let expected_type =
            expected_parsed["type"].as_str().unwrap_or_else(|| panic!("{} parsed type is not a string", case.name));
        match expected_type {
            "info" => {
                let info =
                    outcome.info.as_ref().unwrap_or_else(|| panic!("{} expected parsed info, got none", case.name));
                let encoded = encode_json(info);
                let value: Value = serde_json::from_str(&encoded)
                    .unwrap_or_else(|error| panic!("{} encoded info is not JSON: {error}", case.name));
                assert_eq!(value, expected_parsed["info"], "{} parsed info", case.name);
                assert_eq!(
                    encoded,
                    serde_json::to_string(&expected_parsed["info"]).unwrap(),
                    "{} parsed info key order",
                    case.name
                );
                assert_eq!(
                    diagnostics_value(&outcome.diagnostics),
                    case.value["normalized"]["diagnostics"],
                    "{} parsed diagnostics on success",
                    case.name
                );
            }
            "rejected" => {
                assert!(outcome.info.is_none(), "{} expected no parsed info, got some", case.name);
                let stage = if parsed.is_none() {
                    "malformed"
                } else if matches!(result, Some(Normalized::Rejected { .. })) {
                    "normalize"
                } else {
                    "final"
                };
                assert_eq!(expected_parsed["stage"].as_str(), Some(stage), "{} parsed rejection stage", case.name);
                assert_eq!(
                    diagnostics_value(&outcome.diagnostics),
                    expected_parsed["diagnostics"],
                    "{} parsed diagnostics",
                    case.name
                );
            }
            other => panic!("{} has unknown parsed type {other:?}", case.name),
        }

        assert_eq!(outcome.source, case.name, "{} parsed source", case.name);
    }
}

#[test]
fn the_corpus_covers_every_parsed_outcome() {
    let cases = area("normalize");
    let mut types = Vec::new();
    let mut stages = Vec::new();
    for case in &cases {
        for key in ["group", "input", "normalized", "parsed"] {
            assert!(case.value.get(key).is_some(), "{} is missing {key}", case.name);
        }
        if case.value["group"] != "current" {
            continue;
        }
        let expected_type = case.value["parsed"]["type"]
            .as_str()
            .unwrap_or_else(|| panic!("{} parsed type is not a string", case.name));
        types.push(expected_type.to_owned());
        if expected_type == "rejected" {
            let stage = case.value["parsed"]["stage"]
                .as_str()
                .unwrap_or_else(|| panic!("{} parsed rejection has no stage", case.name));
            stages.push(stage.to_owned());
        }
    }
    for expected in ["info", "rejected"] {
        assert!(types.iter().any(|found| found == expected), "current cases are missing a {expected} parsed outcome");
    }
    for expected in ["malformed", "normalize"] {
        assert!(
            stages.iter().any(|found| found == expected),
            "current rejected cases are missing the {expected} stage"
        );
    }
}
