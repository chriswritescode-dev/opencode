use opencode_config::entry::{Document, Entry, Origin};
use opencode_contract::codec::{decode_json, encode_json};
use opencode_contract::generated::{
    AgentColor, AgentInfoMode, ConfigEntryDocumentInfo, ModelRef, PermissionEffect, ProviderID,
};
use opencode_session::agent::Agents;
use serde_json::Value;

fn document(info: &str) -> Entry {
    Entry::Document(Document {
        path: None,
        info: decode_json::<ConfigEntryDocumentInfo>(info)
            .unwrap_or_else(|error| panic!("{info} failed to decode: {error}")),
        origin: Origin::File,
    })
}

fn ids(agents: &[opencode_contract::generated::AgentInfo]) -> Vec<&str> {
    agents.iter().map(|agent| agent.id.0.as_str()).collect()
}

const EXPECTED_BUILD: &str = r#"{
    "id": "build",
    "name": "Build",
    "request": { "settings": {}, "headers": {}, "body": {} },
    "description": "The default agent. Executes tools based on configured permissions.",
    "mode": "primary",
    "hidden": false,
    "permissions": [
        { "action": "*", "resource": "*", "effect": "allow" },
        { "action": "read", "resource": "*.env", "effect": "ask" },
        { "action": "read", "resource": "*.env.*", "effect": "ask" },
        { "action": "read", "resource": "*.env.example", "effect": "allow" },
        { "action": "question", "resource": "*", "effect": "allow" },
        { "action": "browser", "resource": "*", "effect": "deny" },
        { "action": "skill", "resource": "browser", "effect": "deny" }
    ]
}"#;

#[test]
fn parity_base_config_lists_the_built_in_agents() {
    let agents = Agents::from_entries(&[document(r#"{"agents":{"title":{"disabled":true}}}"#)], "/home/test");
    let list = agents.list();

    assert_eq!(ids(&list), ["build", "general", "explore", "compaction", "summary", "plan"]);
    let build = &list[0];
    assert_eq!(build.id.0, "build");
    assert_eq!(build.mode, AgentInfoMode::Primary);
    assert!(!build.hidden);

    let encoded = serde_json::from_str::<Value>(&encode_json(build)).expect("the build agent encodes as JSON");
    let expected = serde_json::from_str::<Value>(EXPECTED_BUILD).expect("the expected build agent parses");
    assert_eq!(encoded, expected);
}

#[test]
fn config_agent_overrides_and_creates() {
    let entries = [
        document(r#"{"agents":{"title":{"disabled":true}}}"#),
        document(
            r##"{"agents":{
                "build":{"model":"anthropic-test/test-model","description":"Configured build.","mode":"all"},
                "reviewer":{"system":"Review code.","steps":3,"color":"#ff0000","permissions":[{"action":"edit","resource":"*","effect":"deny"}]},
                "general":{"disabled":true}
            }}"##,
        ),
    ];
    let agents = Agents::from_entries(&entries, "/home/test");
    let list = agents.list();
    let build = &list[0];

    assert_eq!(build.id.0, "build");
    assert_eq!(
        build.model,
        Some(ModelRef {
            id: "test-model".to_owned(),
            provider_id: ProviderID("anthropic-test".to_owned()),
            variant: None
        })
    );
    assert_eq!(build.description.as_deref(), Some("Configured build."));
    assert_eq!(build.mode, AgentInfoMode::All);
    assert!(!ids(&list).contains(&"general"));

    let reviewer = agents.select(Some("reviewer")).info.expect("the configured agent exists");
    assert_eq!(reviewer.id.0, "reviewer");
    assert_eq!(reviewer.system.as_deref(), Some("Review code."));
    assert_eq!(reviewer.steps, Some(3.0));
    assert_eq!(reviewer.color, Some(AgentColor("#ff0000".to_owned())));
    assert_eq!(reviewer.permissions.len(), 7);
    assert_eq!(reviewer.permissions[4].action, "edit");
    assert_eq!(reviewer.permissions[4].effect, PermissionEffect::Deny);
    assert_eq!(reviewer.permissions[5].action, "browser");
    assert_eq!(reviewer.permissions[6].action, "skill");

    assert_eq!(ids(&list).last(), Some(&"reviewer"));
}

#[test]
fn select_prefers_configured_default_then_build_then_first_visible() {
    let agents = Agents::from_entries(&[document(r#"{"agents":{"title":{"disabled":true}}}"#)], "/home/test");
    let selection = agents.select(None);
    assert_eq!(selection.id, "build");
    assert_eq!(selection.info.expect("the default agent resolves").name, "Build");

    let configured = Agents::from_entries(
        &[
            document(r#"{"agents":{"reviewer":{"description":"Reviews changes."}}}"#),
            document(r#"{"default_agent":"reviewer"}"#),
        ],
        "/home/test",
    );
    assert_eq!(configured.select(None).id, "reviewer");

    let hidden = Agents::from_entries(&[document(r#"{"default_agent":"summary"}"#)], "/home/test");
    assert_eq!(hidden.select(None).id, "build");
    assert!(hidden.select(Some("summary")).info.expect("the hidden agent resolves").hidden);
    assert_eq!(hidden.select(Some("missing")).info, None);
}

#[test]
fn global_permissions_apply_to_every_agent() {
    let entries = [document(
        r#"{
            "permissions":[{"action":"read","resource":"~/secret","effect":"deny"}],
            "agents":{"build":{"permissions":[{"action":"edit","resource":"$HOME/notes","effect":"ask"}]}}
        }"#,
    )];
    let agents = Agents::from_entries(&entries, "/home/test");

    for agent in agents.list() {
        let browser =
            agent.permissions.iter().position(|rule| rule.action == "browser").expect("every agent hides the browser");
        let global = agent
            .permissions
            .iter()
            .position(|rule| rule.action == "read" && rule.resource == "/home/test/secret")
            .expect("every agent carries the expanded global rule");
        assert_eq!(agent.permissions[global].effect, PermissionEffect::Deny);
        assert!(global < browser);
    }

    let build = agents.select(Some("build")).info.expect("the build agent resolves");
    let configured =
        build.permissions.iter().find(|rule| rule.action == "edit").expect("the configured rule is appended");
    assert_eq!(configured.resource, "/home/test/notes");
    assert_eq!(configured.effect, PermissionEffect::Ask);
}
