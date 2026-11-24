use std::collections::{BTreeMap, BTreeSet};

use opencode_config::variable::{Source, SubstituteInput, substitute};
use opencode_util::path::Flavor;
use opencode_util::temp::tempdir;

use crate::support::{Case, area};

#[test]
fn substitute_cases_match_the_pinned_typescript() {
    let cases = area("substitute");
    assert_eq!(cases.len(), 12, "expected 12 substitute cases, found {}", cases.len());
    for case in &cases {
        verify(case);
    }
}

fn verify(case: &Case) {
    let root = tempdir().unwrap();
    let root_text = root.path().to_string_lossy().into_owned();
    let expand = |text: &str| text.replace("{{root}}", &root_text);

    for (file, content) in case.value["files"].as_object().unwrap() {
        let path = root.path().join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content.as_str().unwrap()).unwrap();
    }

    let text = expand(case.value["text"].as_str().unwrap());
    let env: BTreeMap<String, String> = case.value["env"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(key, value)| (key.clone(), expand(value.as_str().unwrap())))
        .collect();
    let home = root.path().join("home");
    let source = &case.value["source"];
    let mut references = BTreeSet::new();
    let output = match source["type"].as_str().unwrap() {
        "path" => {
            let path = root.path().join(source["path"].as_str().unwrap());
            substitute(
                &SubstituteInput { source: Source::Path(&path), text: &text, env: Some(&env), home: &home },
                &mut references,
                |_| None,
            )
        }
        "virtual" => {
            let file = source["source"].as_str().unwrap();
            let dir = root.path().join(source["dir"].as_str().unwrap());
            substitute(
                &SubstituteInput {
                    source: Source::Virtual { source: file, dir: &dir },
                    text: &text,
                    env: Some(&env),
                    home: &home,
                },
                &mut references,
                |_| None,
            )
        }
        other => panic!("{} has unknown source type {other:?}", case.name),
    };

    assert_eq!(output, case.value["output"].as_str().unwrap(), "{} substitute output", case.name);

    let expected: Vec<String> =
        case.value["references"].as_array().unwrap().iter().map(|value| value.as_str().unwrap().to_owned()).collect();
    let mut actual: Vec<String> = references
        .iter()
        .map(|reference| Flavor::NATIVE.relative(&root_text, reference.to_str().unwrap()).replace('\\', "/"))
        .collect();
    actual.sort();
    assert_eq!(actual, expected, "{} substitute references", case.name);
}
