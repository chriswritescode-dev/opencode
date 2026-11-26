use std::collections::HashMap;

use opencode_contract::generated::{PermissionEffect, PermissionRule};
use opencode_fs::access::{Access, AccessError, ExternalDirectory, Target};
use opencode_shell::permissions::{DirectoryResolver, Input, Request, requests};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TestExternal {
    action: String,
    directory: String,
    resource: String,
    save: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TestTarget {
    absolute: String,
    resource: String,
    #[serde(default)]
    external_directory: Option<TestExternal>,
}

#[derive(Deserialize)]
struct TestRule {
    action: String,
    resource: String,
    effect: String,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    command: String,
    program: String,
    once: bool,
    target: TestTarget,
    resolved: HashMap<String, TestTarget>,
    rules: Vec<TestRule>,
    #[serde(default)]
    expected: Option<Vec<Request>>,
    #[serde(default)]
    error: Option<String>,
}

impl TestTarget {
    fn into_target(self) -> Target {
        Target {
            absolute: self.absolute,
            resource: self.resource,
            external_directory: self.external_directory.map(|external| {
                assert_eq!(external.action, "external_directory");
                ExternalDirectory {
                    action: "external_directory",
                    directory: external.directory,
                    resource: external.resource,
                    save: external.save,
                }
            }),
        }
    }
}

struct TableResolver {
    targets: HashMap<String, Target>,
}

impl DirectoryResolver for TableResolver {
    async fn resolve(&self, directory: &str, _base: &Target) -> Result<Target, AccessError> {
        Ok(self.targets.get(directory).cloned().unwrap_or_else(|| Target {
            absolute: directory.to_owned(),
            resource: directory.to_owned(),
            external_directory: None,
        }))
    }
}

#[tokio::test]
async fn replays_the_permissions_corpus() {
    let corpus = std::fs::read_to_string(format!("{}/tests/corpus/permissions.json", env!("CARGO_MANIFEST_DIR")))
        .expect("permissions corpus");
    let cases: Vec<Case> = serde_json::from_str(&corpus).expect("permissions corpus json");
    let mut mismatches = Vec::new();
    for case in cases {
        let target = case.target.into_target();
        let resolver = TableResolver {
            targets: case.resolved.into_iter().map(|(path, target)| (path, target.into_target())).collect(),
        };
        let rules: Vec<PermissionRule> = case
            .rules
            .into_iter()
            .map(|rule| PermissionRule {
                action: rule.action,
                resource: rule.resource,
                effect: match rule.effect.as_str() {
                    "allow" => PermissionEffect::Allow,
                    "deny" => PermissionEffect::Deny,
                    "ask" => PermissionEffect::Ask,
                    other => panic!("{}: unknown effect {other}", case.name),
                },
            })
            .collect();
        let actual = requests(
            &resolver,
            &Input { command: &case.command, program: &case.program, target: &target, once: case.once },
            &rules,
        )
        .await;
        let matched = match actual {
            Ok(actual) => case.expected.as_ref().is_some_and(|expected| expected == &actual),
            Err(opencode_shell::permissions::Error::Unverifiable { reason }) => {
                case.error.as_deref() == Some(reason.as_str())
            }
            Err(error) => {
                mismatches.push(format!("{}: unexpected error {error}", case.name));
                continue;
            }
        };
        if !matched {
            mismatches.push(format!(
                "{}: {:?} once={}\n  expected {:?} / {:?}\n  actual differs",
                case.name, case.command, case.once, case.expected, case.error,
            ));
        }
    }
    assert!(mismatches.is_empty(), "{} permissions cases mismatch:\n{}", mismatches.len(), mismatches.join("\n"));
}

#[tokio::test]
async fn resolves_directory_targets_through_the_filesystem_access() {
    let access = Access::new("/workspace", "/workspace", "/corpus/home");
    let target = Target { absolute: "/workspace".to_owned(), resource: ".".to_owned(), external_directory: None };
    let actual = requests(
        &access,
        &Input { command: "cd /outside && git status", program: "/bin/bash", target: &target, once: false },
        &[],
    )
    .await
    .expect("requests");
    assert_eq!(actual.len(), 2);
    assert_eq!(actual[0].action, "external_directory");
    assert_eq!(actual[0].resources, vec!["/outside/*"]);
    assert_eq!(actual[0].save.as_deref(), Some(&["/outside/*".to_owned()][..]));
    assert_eq!(actual[1].action, "shell");
    assert_eq!(actual[1].resources, vec!["git status"]);
}
