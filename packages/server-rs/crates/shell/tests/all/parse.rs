use std::collections::BTreeMap;
use std::io::Read;

use flate2::read::GzDecoder;
use opencode_shell::parse::{Options, Parsed, scan_with};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct User {
    name: String,
    home: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    name: String,
    source: String,
    command: String,
    shell: String,
    cwd: String,
    tilde_home: Option<String>,
    system_home: Option<String>,
    environment: BTreeMap<String, String>,
    passwd: BTreeMap<String, String>,
    current_user: Option<User>,
    #[serde(default)]
    result: Option<Parsed>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize)]
struct ParityCase {
    command: String,
    shell: String,
    expected: Parsed,
}

#[test]
fn replays_the_parser_corpus() {
    let corpus = std::fs::read_to_string(format!("{}/tests/corpus/parse.json", env!("CARGO_MANIFEST_DIR")))
        .expect("parse corpus");
    let cases: Vec<Case> = serde_json::from_str(&corpus).expect("parse corpus json");
    let mismatches: Vec<String> = cases
        .iter()
        .filter_map(|case| {
            let options = Options {
                tilde_home: case.tilde_home.clone(),
                system_home: case.system_home.clone(),
                environment: case.environment.clone(),
                passwd: case.passwd.clone(),
                current_user: case.current_user.as_ref().map(|user| (user.name.clone(), user.home.clone())),
            };
            let actual = scan_with(&case.command, &case.shell, &case.cwd, &options);
            match actual {
                Ok(actual) if case.result.as_ref().is_some_and(|expected| expected == &actual) => None,
                Err(error) if case.error.as_deref() == Some(error.reason.as_str()) => None,
                Ok(actual) => Some(format!(
                    "{} ({}): {:?} {:?}\n  expected {}\n  actual   {}",
                    case.name,
                    case.source,
                    case.shell,
                    case.command,
                    serde_json::to_string(&case.result).unwrap_or_default(),
                    serde_json::to_string(&actual).unwrap_or_default(),
                )),
                Err(error) => Some(format!(
                    "{} ({}): {:?} {:?}\n  expected {:?}\n  actual   {:?}",
                    case.name, case.source, case.shell, case.command, case.error, error.reason,
                )),
            }
        })
        .take(20)
        .collect();
    assert!(
        mismatches.is_empty(),
        "{} of {} parse cases mismatch:\n{}",
        mismatches.len(),
        cases.len(),
        mismatches.join("\n"),
    );
}

#[test]
fn replays_the_parse_parity_corpus() {
    let file = std::fs::File::open(format!("{}/tests/corpus/parse-parity.jsonl.gz", env!("CARGO_MANIFEST_DIR")))
        .expect("parse parity corpus");
    let mut text = String::new();
    GzDecoder::new(file).read_to_string(&mut text).expect("parse parity jsonl");
    let options = Options {
        tilde_home: Some("/corpus/home".to_owned()),
        system_home: Some("/corpus/home".to_owned()),
        environment: BTreeMap::new(),
        passwd: BTreeMap::new(),
        current_user: None,
    };
    let mut total = 0;
    let mut mismatches = Vec::new();
    for line in text.lines().filter(|line| !line.is_empty()) {
        total += 1;
        let case: ParityCase = serde_json::from_str(line).expect("parse parity case");
        let actual = scan_with(&case.command, &case.shell, "/workspace", &options);
        match actual {
            Ok(actual) if actual == case.expected => {
                for command in &actual.commands {
                    if !is_assignment(&command.resource) {
                        assert!(
                            opencode_config::wildcard::matches(&command.resource, &command.save, false),
                            "{} {:?}: resource {:?} does not match save {:?}",
                            case.shell,
                            case.command,
                            command.resource,
                            command.save,
                        );
                    }
                }
            }
            Ok(actual) => {
                if mismatches.len() < 20 {
                    mismatches.push(format!(
                        "{:?} {:?}\n  expected {}\n  actual   {}",
                        case.shell,
                        case.command,
                        serde_json::to_string(&case.expected).unwrap_or_default(),
                        serde_json::to_string(&actual).unwrap_or_default(),
                    ));
                }
            }
            Err(error) => {
                if mismatches.len() < 20 {
                    mismatches.push(format!("{:?} {:?}\n  error {error}", case.shell, case.command));
                }
            }
        }
    }
    assert!(total > 20_000, "parse parity corpus has only {total} commands");
    assert!(mismatches.is_empty(), "{} of {total} parity cases mismatch:\n{}", mismatches.len(), mismatches.join("\n"));
}

#[test]
fn scans_with_options_built_from_the_environment() {
    let options = Options::from_env(std::env::vars());
    let parsed = scan_with("git status && cd /tmp && pwd", "/bin/bash", "/workspace", &options).expect("parses");
    assert_eq!(
        parsed.commands,
        vec![
            opencode_shell::parse::ParsedCommand { resource: "git status".to_owned(), save: "git status *".to_owned() },
            opencode_shell::parse::ParsedCommand { resource: "pwd".to_owned(), save: "pwd *".to_owned() },
        ]
    );
    assert_eq!(parsed.directories, vec!["/tmp"]);
}

#[test]
fn leaves_tilde_unexpanded_when_the_home_is_unset() {
    let options = Options {
        tilde_home: None,
        system_home: Some("/corpus/home".to_owned()),
        environment: BTreeMap::new(),
        passwd: BTreeMap::new(),
        current_user: None,
    };
    let parsed = scan_with("cd ~/src; cd '~/quoted'", "/bin/bash", "/workspace", &options).expect("parses");
    assert_eq!(parsed.directories, vec!["~/src", "./~/quoted"]);
}

fn is_assignment(resource: &str) -> bool {
    let mut chars = resource.chars();
    let Some(first) = chars.next() else { return false };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    for char in chars {
        if char == '=' {
            return true;
        }
        if !(char.is_ascii_alphanumeric() || char == '_') {
            return false;
        }
    }
    false
}
