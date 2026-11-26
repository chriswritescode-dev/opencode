use std::fs;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use opencode_fs::{bom, mime, path};
use serde_json::Value;

const ROOT: &str = "/repo";
const HOME: &str = "/home/agent";

#[test]
fn paths_corpus_matches_expected_output() {
    for case in cases("paths") {
        let name = field(&case, "name");
        let actual = match field(&case, "kind") {
            "contains" => Value::Bool(path::contains(&templated(&case, "parent"), &templated(&case, "child"))),
            "resolve" => Value::String(path::resolve(&templated(&case, "path"))),
            "resolvePath" => Value::String(path::resolve_path(
                &templated(&case, "directory"),
                &templated(&case, "input"),
                &templated(&case, "home"),
            )),
            kind => panic!("case {name}: unknown kind {kind}"),
        };
        let expected = match &case["expected"] {
            Value::String(value) => Value::String(template(value)),
            value => value.clone(),
        };
        assert_eq!(actual, expected, "case {name}");
    }
}

#[test]
fn mime_corpus_matches_expected_output() {
    for case in cases("mime") {
        let name = field(&case, "name");
        let actual = match field(&case, "kind") {
            "detect" => Value::String(mime::detect(&bytes(&case, "bytes")).to_owned()),
            "media" => match mime::detect_media(&bytes(&case, "bytes")) {
                Some(detected) => Value::String(detected.to_owned()),
                None => Value::Null,
            },
            "textual" => Value::Bool(mime::is_textual(field(&case, "mime"))),
            "lookup" => Value::String(mime::lookup(&templated(&case, "path")).to_owned()),
            kind => panic!("case {name}: unknown kind {kind}"),
        };
        assert_eq!(actual, case["expected"], "case {name}");
    }
}

#[test]
fn bom_corpus_matches_expected_output() {
    for case in cases("bom") {
        let name = field(&case, "name");
        match field(&case, "kind") {
            "sync" => {
                let synced = bom::sync_bytes(&bytes(&case, "input"), case["bom"].as_bool().expect("bom is a bool"));
                assert_eq!(Value::String(synced.text), case["text"], "case {name}: text");
                assert_eq!(
                    synced.bytes.map(|bytes| STANDARD.encode(bytes)),
                    expected_bytes(&case),
                    "case {name}: bytes"
                );
            }
            "split" => {
                let (found, text) = bom::split(field(&case, "text"));
                assert_eq!(Value::Bool(found), case["bom"], "case {name}: bom");
                assert_eq!(Value::String(text.to_owned()), case["expected"], "case {name}: text");
            }
            "join" => {
                let joined = bom::join(field(&case, "text"), case["bom"].as_bool().expect("bom is a bool"));
                assert_eq!(Value::String(joined), case["expected"], "case {name}");
            }
            kind => panic!("case {name}: unknown kind {kind}"),
        }
    }
}

fn cases(area: &str) -> Vec<Value> {
    let path = format!("{}/tests/corpus/{area}.json", env!("CARGO_MANIFEST_DIR"));
    let text = fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path}: {error}"));
    let cases: Vec<Value> = serde_json::from_str(&text).unwrap_or_else(|error| panic!("parse {path}: {error}"));
    assert!(!cases.is_empty(), "{area} corpus is empty");
    cases
}

fn field<'a>(case: &'a Value, key: &str) -> &'a str {
    case[key].as_str().unwrap_or_else(|| panic!("case {}: {key} is a string", case["name"]))
}

fn templated(case: &Value, key: &str) -> String {
    template(field(case, key))
}

fn template(value: &str) -> String {
    value.replace("{{root}}", ROOT).replace("{{home}}", HOME)
}

fn bytes(case: &Value, key: &str) -> Vec<u8> {
    STANDARD.decode(field(case, key)).unwrap_or_else(|error| panic!("case {}: {key}: {error}", case["name"]))
}

fn expected_bytes(case: &Value) -> Option<String> {
    case["bytes"].as_str().map(str::to_owned)
}
