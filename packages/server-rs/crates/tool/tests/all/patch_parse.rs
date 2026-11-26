use opencode_tool::patch::{UpdateFileChunk, derive, parse};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct Case {
    name: String,
    operation: String,
    #[serde(default)]
    input: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    chunks: Vec<UpdateFileChunk>,
    #[serde(default)]
    original: String,
    #[serde(default)]
    expected: Option<Value>,
    #[serde(default)]
    error: Option<String>,
}

#[test]
fn patch_parse_corpus_matches() {
    let cases: Vec<Case> =
        serde_json::from_str(include_str!("../corpus/patch_parse.json")).expect("the patch parse corpus is valid JSON");
    assert!(!cases.is_empty());
    for case in cases {
        match case.operation.as_str() {
            "parse" => match parse(&case.input) {
                Ok(hunks) => assert_eq!(
                    serde_json::to_value(&hunks).expect("the hunks serialise"),
                    case.expected.unwrap_or(Value::Null),
                    "case {}",
                    case.name
                ),
                Err(error) => assert_eq!(Some(error.to_string()), case.error, "case {}", case.name),
            },
            "derive" => match derive(&case.path, &case.chunks, &case.original) {
                Ok(update) => assert_eq!(
                    serde_json::to_value(&update).expect("the file update serialises"),
                    case.expected.unwrap_or(Value::Null),
                    "case {}",
                    case.name
                ),
                Err(error) => assert_eq!(Some(error.to_string()), case.error, "case {}", case.name),
            },
            operation => panic!("case {} has an unknown operation {operation}", case.name),
        }
    }
}
