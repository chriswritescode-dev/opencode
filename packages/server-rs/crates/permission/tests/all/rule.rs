use opencode_contract::generated::{PermissionEffect, PermissionRule};
use opencode_permission::rule::{Platform, evaluate, evaluate_on, ignores_case, merge};

fn rule(action: &str, resource: &str, effect: PermissionEffect) -> PermissionRule {
    PermissionRule { action: action.to_string(), resource: resource.to_string(), effect }
}

fn allow(action: &str, resource: &str) -> PermissionRule {
    rule(action, resource, PermissionEffect::Allow)
}

fn deny(action: &str, resource: &str) -> PermissionRule {
    rule(action, resource, PermissionEffect::Deny)
}

#[test]
fn last_matching_rule_wins_across_rulesets() {
    let first = [allow("read", "*")];
    let second = [deny("read", "secret/*")];
    let third = [allow("read", "secret/public")];
    let result = evaluate_on(Platform::Other, "read", "secret/public", &[&first, &second, &third]);
    assert_eq!(result, allow("read", "secret/public"));

    let result = evaluate_on(Platform::Other, "read", "secret/private", &[&first, &second, &third]);
    assert_eq!(result, deny("read", "secret/*"));
}

#[test]
fn no_match_returns_the_ask_default() {
    let result = evaluate_on(Platform::Other, "read", "docs/readme.md", &[]);
    assert_eq!(result, rule("read", "*", PermissionEffect::Ask));
}

#[test]
fn matches_action_wildcards_and_resource_globs() {
    let rules = [allow("*", "build*")];
    let result = evaluate("skill", "build-all", &[&rules]);
    assert_eq!(result, allow("*", "build*"));

    let result = evaluate("skill", "deploy", &[&rules]);
    assert_eq!(result, rule("skill", "*", PermissionEffect::Ask));
}

#[test]
fn normalizes_backslash_resources() {
    let rules = [allow("read", "dir/*")];
    let result = evaluate_on(Platform::Other, "read", "dir\\nested\\file.txt", &[&rules]);
    assert_eq!(result, allow("read", "dir/*"));
}

#[test]
fn ignores_case_only_for_path_actions_on_macos() {
    for action in ["external_directory", "read", "edit"] {
        assert!(ignores_case(action, Platform::Macos));
        assert!(!ignores_case(action, Platform::Other));
    }
    assert!(!ignores_case("skill", Platform::Macos));
    assert!(!ignores_case("bash", Platform::Macos));
}

#[test]
fn merge_concatenates_in_order() {
    let first = [allow("read", "*"), deny("edit", "*")];
    let second = [allow("skill", "build")];
    assert_eq!(merge(&[&first, &second]), [allow("read", "*"), deny("edit", "*"), allow("skill", "build")]);
}
