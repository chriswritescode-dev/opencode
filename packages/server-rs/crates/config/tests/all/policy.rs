use std::path::PathBuf;

use opencode_config::entry::{Document, Entry, Origin, latest};
use opencode_config::policy::{self, Statement};
use opencode_config::seams::CredentialValue;
use opencode_config::wildcard;
use opencode_contract::codec::decode_json;
use opencode_contract::generated::{
    ConfigEntryDocumentInfo, ConfigEntryDocumentInfoPoliciesItemAction, ConfigEntryDocumentInfoPoliciesItemEffect,
};

const DENY_TOOL: &str = r#"{"policies":[{"action":"tool.use","resource":"*","effect":"deny"}]}"#;

fn document(info: &str, origin: Origin) -> Entry {
    Entry::Document(Document {
        path: None,
        info: decode_json::<ConfigEntryDocumentInfo>(info)
            .unwrap_or_else(|error| panic!("{info} failed to decode: {error}")),
        origin,
    })
}

fn integration(organization: Option<&str>) -> Origin {
    Origin::Integration {
        integration_id: "opencode".to_string(),
        connection: serde_json::Value::Null,
        organization: organization.map(str::to_string),
    }
}

fn resource_statement(resource: &str, effect: ConfigEntryDocumentInfoPoliciesItemEffect) -> Statement {
    Statement {
        action: ConfigEntryDocumentInfoPoliciesItemAction::ToolUse,
        resource: resource.to_string(),
        effect,
        message: String::new(),
    }
}

fn credential(metadata: &str) -> CredentialValue {
    CredentialValue { key: None, metadata: serde_json::from_str(metadata).expect("metadata is a JSON object") }
}

#[test]
fn statements_follow_reverse_load_order_and_skip_directories() {
    let entries = vec![
        document(r#"{"policies":[{"action":"tool.use","resource":"early","effect":"allow"}]}"#, Origin::File),
        Entry::Directory { path: PathBuf::from("/tmp/config") },
        document(r#"{"policies":[{"action":"tool.use","resource":"late","effect":"deny"}]}"#, Origin::Managed),
    ];

    let resources: Vec<String> = policy::statements(&entries).into_iter().map(|statement| statement.resource).collect();

    assert_eq!(resources, ["late".to_string(), "early".to_string()]);
}

#[test]
fn statements_carry_the_message_of_their_origin() {
    let entries = vec![
        document(DENY_TOOL, Origin::File),
        document(DENY_TOOL, integration(Some("Acme"))),
        document(DENY_TOOL, integration(None)),
    ];

    let messages: Vec<String> =
        policy::statements(&entries).iter().map(|statement| statement.message.clone()).collect();

    assert_eq!(
        messages,
        ["Blocked by your organization's policy", "Blocked by Acme's policy", "Blocked by configuration policy",]
    );
}

#[test]
fn the_last_matching_statement_wins() {
    let ordered = vec![
        resource_statement("bash", ConfigEntryDocumentInfoPoliciesItemEffect::Allow),
        resource_statement("bash", ConfigEntryDocumentInfoPoliciesItemEffect::Deny),
    ];

    assert_eq!(
        policy::matching(&ordered, ConfigEntryDocumentInfoPoliciesItemAction::ToolUse, "bash"),
        Some(&ordered[1])
    );
}

#[test]
fn the_earliest_loaded_document_decides() {
    let entries = vec![
        document(r#"{"policies":[{"action":"tool.use","resource":"bash","effect":"allow"}]}"#, Origin::File),
        document(r#"{"policies":[{"action":"tool.use","resource":"bash","effect":"deny"}]}"#, Origin::Managed),
    ];
    let policies = policy::statements(&entries);

    assert_eq!(
        policy::decision(&policies, ConfigEntryDocumentInfoPoliciesItemAction::ToolUse, "bash"),
        ConfigEntryDocumentInfoPoliciesItemEffect::Allow
    );
}

#[test]
fn unmatched_actions_and_resources_are_allowed() {
    let policies = policy::statements(&[document(
        r#"{"policies":[{"action":"tool.use","resource":"bash","effect":"deny"}]}"#,
        Origin::File,
    )]);

    assert_eq!(
        policy::decision(&policies, ConfigEntryDocumentInfoPoliciesItemAction::ToolUse, "bash"),
        ConfigEntryDocumentInfoPoliciesItemEffect::Deny
    );
    assert_eq!(
        policy::decision(&policies, ConfigEntryDocumentInfoPoliciesItemAction::McpUse, "bash"),
        ConfigEntryDocumentInfoPoliciesItemEffect::Allow
    );
    assert_eq!(
        policy::decision(&policies, ConfigEntryDocumentInfoPoliciesItemAction::ToolUse, "bashx"),
        ConfigEntryDocumentInfoPoliciesItemEffect::Allow
    );
    assert_eq!(
        policy::decision(&[], ConfigEntryDocumentInfoPoliciesItemAction::ToolUse, "bash"),
        ConfigEntryDocumentInfoPoliciesItemEffect::Allow
    );
}

#[test]
fn integration_resources_include_a_known_account() {
    assert_eq!(policy::integration_resource("opencode", Some("acct_1")), "opencode#acct_1");
    assert_eq!(policy::integration_resource("opencode", None), "opencode");
    assert_eq!(policy::integration_resource("https://example.com", None), "https://example.com");
}

#[test]
fn an_empty_organization_or_account_counts_as_absent() {
    let policies = policy::statements(&[document(DENY_TOOL, integration(Some("")))]);

    assert_eq!(policies[0].message, "Blocked by your organization's policy");
    assert_eq!(policy::integration_resource("opencode", Some("")), "opencode");
}

#[test]
fn account_prefers_the_organization_then_the_account_id() {
    assert_eq!(policy::account(&credential(r#"{"orgID":"org_1","accountID":"acct_1"}"#)), Some("org_1".to_string()));
    assert_eq!(policy::account(&credential(r#"{"accountID":"acct_1"}"#)), Some("acct_1".to_string()));
    assert_eq!(policy::account(&credential(r#"{"orgID":"","accountID":"acct_1"}"#)), Some("acct_1".to_string()));
    assert_eq!(policy::account(&credential(r#"{"orgID":"","accountID":""}"#)), None);
    assert_eq!(policy::account(&credential(r#"{"orgID":5,"accountID":null}"#)), None);
    assert_eq!(policy::account(&credential("{}")), None);
}

#[test]
fn a_document_can_keep_only_its_policies() {
    let Entry::Document(document) = document(
        r#"{"shell":"bash","policies":[{"action":"tool.use","resource":"bash","effect":"deny"}]}"#,
        Origin::Managed,
    ) else {
        panic!("expected a document entry");
    };
    let stripped = document.policies_only();

    assert_eq!(stripped.origin, Origin::Managed);
    assert!(stripped.info.shell.is_none());
    let policies = policy::statements(&[Entry::Document(stripped)]);
    assert_eq!(policies.len(), 1);
    assert_eq!(policies[0].effect, ConfigEntryDocumentInfoPoliciesItemEffect::Deny);
}

#[test]
fn a_document_without_policies_strips_to_empty_info() {
    let Entry::Document(document) = document(r#"{"shell":"bash"}"#, Origin::File) else {
        panic!("expected a document entry");
    };

    assert_eq!(document.policies_only().info, decode_json::<ConfigEntryDocumentInfo>("{}").unwrap());
}

#[test]
fn latest_picks_the_last_document_setting() {
    let entries = vec![
        document(r#"{"shell":"zsh"}"#, Origin::File),
        Entry::Directory { path: PathBuf::from("/tmp/config") },
        document(r#"{"username":"ada"}"#, Origin::Managed),
        document(r#"{"shell":"bash"}"#, Origin::Managed),
    ];

    assert_eq!(latest(&entries, |info| info.shell.clone()), Some("bash".to_string()));
    assert_eq!(latest(&entries, |info| info.timezone.clone()), None);
}

#[test]
fn wildcard_matches_literal_patterns_only() {
    assert!(wildcard::matches("shell:git", "shell:git", false));
    assert!(!wildcard::matches("shell:gits", "shell:git", false));
    assert!(!wildcard::matches("", "shell:git", false));
}

#[test]
fn wildcard_star_spans_newlines() {
    assert!(wildcard::matches("a\nb", "a*b", false));
    assert!(wildcard::matches("shell:git status --short", "shell:*", false));
    assert!(wildcard::matches("dir/file", "*", false));
}

#[test]
fn wildcard_question_matches_one_character() {
    assert!(wildcard::matches("abc", "a?c", false));
    assert!(!wildcard::matches("ac", "a?c", false));
    assert!(!wildcard::matches("abbc", "a?c", false));
}

#[test]
fn wildcard_escapes_regular_expression_metacharacters() {
    assert!(wildcard::matches("file.ts", "file.ts", false));
    assert!(!wildcard::matches("fileXts", "file.ts", false));
    assert!(wildcard::matches("a+b", "a+b", false));
    assert!(!wildcard::matches("aab", "a+b", false));
    assert!(wildcard::matches("[x]{1}|(y)$^", "[x]{1}|(y)$^", false));
}

#[test]
fn wildcard_trailing_space_star_is_optional() {
    assert!(wildcard::matches("shell:git status", "shell:git *", false));
    assert!(wildcard::matches("shell:git", "shell:git *", false));
    assert!(!wildcard::matches("shell:gitx", "shell:git *", false));
    assert!(wildcard::matches("shell:git st\natus", "shell:git *", false));
}

#[test]
fn wildcard_normalises_backslashes() {
    assert!(wildcard::matches("dir\\file", "dir/file", false));
    assert!(wildcard::matches("dir/file", "dir\\file", false));
    assert!(wildcard::matches("dir\\nested\\file", "dir/*", false));
}

#[test]
fn wildcard_ignore_case_only_applies_when_requested() {
    assert_eq!(wildcard::matches("SHELL", "shell", false), cfg!(windows));
    assert!(wildcard::matches("SHELL", "shell", true));
    assert!(wildcard::matches("File.TS", "file.ts", true));
}
