use std::path::Path;

use opencode_catalog::oauth::page::{BootstrapOptions, CallbackPageOptions, bootstrap, error, success};
use opencode_catalog::oauth::redirect::{Pasted, pasted};
use serde_json::Value;

pub fn area(name: &str) -> Vec<(String, Value)> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("corpus").join(format!("{name}.json"));
    let input =
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    let value: Value =
        serde_json::from_str(&input).unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()));
    value
        .as_object()
        .unwrap_or_else(|| panic!("{} is not a JSON object", path.display()))
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

fn options(value: &Value) -> Option<CallbackPageOptions> {
    if value.is_null() {
        return None;
    }
    Some(CallbackPageOptions { provider: value["provider"].as_str().map(str::to_owned), auto_close: None })
}

fn pairs(value: &Value) -> Vec<(String, String)> {
    value
        .as_array()
        .expect("the params array")
        .iter()
        .map(|pair| {
            let pair = pair.as_array().expect("a [key, value] pair");
            (pair[0].as_str().expect("a param key").to_owned(), pair[1].as_str().expect("a param value").to_owned())
        })
        .collect()
}

#[test]
fn oauth_corpus_matches_the_pinned_typescript() {
    let cases = area("oauth");
    assert_eq!(cases.len(), 13, "the corpus holds every pinned oauth case");

    for (name, case) in &cases {
        match case["call"].as_str().expect("a call name") {
            "success" => {
                let expected = case["html"].as_str().expect("html");
                assert_eq!(success(options(&case["options"])), expected, "case {name}");
            }
            "error" => {
                let detail = case["detail"].as_str().expect("detail");
                let expected = case["html"].as_str().expect("html");
                assert_eq!(error(detail, options(&case["options"])), expected, "case {name}");
            }
            "bootstrap" => {
                let input = BootstrapOptions {
                    token_path: case["tokenPath"].as_str().expect("tokenPath").to_owned(),
                    provider: Some(case["provider"].as_str().expect("provider").to_owned()),
                };
                let expected = case["html"].as_str().expect("html");
                assert_eq!(bootstrap(input), expected, "case {name}");
            }
            "pasted" => {
                let text = case["text"].as_str().expect("text");
                let state = case["state"].as_str().expect("state");
                let result = &case["result"];
                let actual = pasted(text, state);
                match result["kind"].as_str().expect("a result kind") {
                    "failure" => {
                        let message = result["message"].as_str().expect("message");
                        assert_eq!(actual.unwrap_err().to_string(), message, "case {name}");
                    }
                    "code" => {
                        let expected = Pasted::Code {
                            code: result["code"].as_str().expect("code").to_owned(),
                            params: pairs(&result["params"]),
                        };
                        assert_eq!(actual, Ok(expected), "case {name}");
                    }
                    "error" => {
                        let expected = Pasted::Error {
                            error: result["error"].as_str().expect("error").to_owned(),
                            params: pairs(&result["params"]),
                        };
                        assert_eq!(actual, Ok(expected), "case {name}");
                    }
                    kind => panic!("case {name} has an unknown pasted kind {kind}"),
                }
            }
            call => panic!("case {name} has an unknown call {call}"),
        }
    }
}
