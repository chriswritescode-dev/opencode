use std::path::PathBuf;
use std::sync::Arc;

use opencode_config::entry::{Document, Entry, Origin};
use opencode_config::integration;
use opencode_config::options::{ConfigLocation, ConfigOptions, SystemOption};
use opencode_config::paths::GlobalPaths;
use opencode_config::seams::NoKv;
use opencode_config::service::{ConfigService, ServiceDeps};
use opencode_config::wellknown::WellKnown;
use opencode_contract::codec::encode_json;
use opencode_contract::generated::{
    ConfigEntryDocumentInfo, ConfigEntryDocumentInfoPoliciesItemAction, ConfigEntryDocumentInfoPoliciesItemEffect,
};
use opencode_util::temp::TempDir;
use serde_json::{Value, json};

#[test]
fn drops_policy_statements_this_version_does_not_know() {
    let info = integration::remote(&json!({
        "policies": [
            { "action": "tool.use", "resource": "bash", "effect": "deny" },
            { "action": "future.use", "resource": "*", "effect": "deny" },
            { "action": "tool.use", "resource": "edit", "effect": "future" }
        ]
    }))
    .unwrap();

    let policies = info.policies.unwrap();
    assert_eq!(policies.len(), 1);
    assert_eq!(policies[0].action, ConfigEntryDocumentInfoPoliciesItemAction::ToolUse);
    assert_eq!(policies[0].resource, "bash");
    assert_eq!(policies[0].effect, ConfigEntryDocumentInfoPoliciesItemEffect::Deny);
}

#[test]
fn fails_on_a_malformed_known_statement() {
    assert!(integration::remote(&json!({ "policies": [{ "action": "tool.use", "effect": "deny" }] })).is_err());
    assert!(integration::remote(&json!({ "policies": { "action": "tool.use" } })).is_err());
}

#[test]
fn ignores_keys_outside_the_allow_list() {
    let info = integration::remote(&json!({
        "providers": { "acme": { "name": "Acme" } },
        "shell": 42,
        "plugins": "not a list",
        "instructions": [1, 2],
        "experimental": { "policies": "not a list" }
    }))
    .unwrap();

    assert!(info.providers.is_some());
    assert!(info.shell.is_none());
    assert!(info.plugins.is_none());
    assert!(info.instructions.is_none());
    assert!(info.experimental.is_none());
    assert!(info.policies.is_none());
}

#[test]
fn strips_credentials_and_env_from_providers_models_and_variants() {
    let info = integration::remote(&json!({
        "providers": {
            "acme": {
                "env": ["ACME_KEY"],
                "settings": {
                    "apiKey": "secret",
                    "authToken": "token",
                    "accessToken": "access",
                    "baseURL": "https://example.com"
                },
                "models": {
                    "one": {
                        "settings": { "apiKey": "secret", "baseURL": "https://example.com/one" },
                        "variants": [
                            {
                                "id": "fast",
                                "settings": {
                                    "apiKey": "secret",
                                    "authToken": "token",
                                    "accessToken": "access",
                                    "reasoning": "high"
                                }
                            }
                        ]
                    }
                }
            }
        }
    }))
    .unwrap();

    let document = integration::make(json!({ "type": "credential", "id": "connection" }), None, info);
    let value = to_value(&document.info);

    assert_eq!(value["providers"]["acme"].get("env"), None);
    assert_eq!(value["providers"]["acme"]["settings"], json!({ "baseURL": "https://example.com" }));
    assert_eq!(
        value["providers"]["acme"]["models"]["one"]["settings"],
        json!({ "baseURL": "https://example.com/one" })
    );
    assert_eq!(value["providers"]["acme"]["models"]["one"]["variants"][0]["settings"], json!({ "reasoning": "high" }));
    assert!(!encode_json(&document.info).contains("secret"));
}

#[test]
fn keeps_only_remote_mcp_servers() {
    let info = integration::remote(&json!({
        "mcp": {
            "timeout": { "startup": 1000 },
            "servers": {
                "local": { "type": "local", "command": ["node", "server.js"] },
                "remote": { "type": "remote", "url": "https://example.com/mcp" }
            }
        }
    }))
    .unwrap();

    let document = integration::make(json!({ "type": "credential", "id": "connection" }), None, info);
    let value = to_value(&document.info);

    assert_eq!(
        value["mcp"],
        json!({ "servers": { "remote": { "type": "remote", "url": "https://example.com/mcp" } } })
    );
}

#[tokio::test]
async fn ranks_the_document_below_managed_policies() {
    let workspace = workspace();
    std::fs::write(
        workspace.system.join("opencode.json"),
        r#"{"shell":"managed","policies":[{"action":"tool.use","resource":"managed","effect":"deny"}]}"#,
    )
    .unwrap();
    let service = start(&workspace).await;

    let document = integration::make(
        json!({ "type": "credential", "id": "connection" }),
        None,
        integration::remote(&json!({
            "providers": { "acme": { "name": "Acme" } },
            "policies": [{ "action": "tool.use", "resource": "integrated", "effect": "allow" }]
        }))
        .unwrap(),
    );
    service.integration("opencode", Some(document)).await;

    let documents = documents(&service);
    assert_eq!(documents[0].origin, Origin::Managed);
    assert!(documents[0].info.policies.is_some(), "the managed policies rank first");
    assert!(matches!(documents[1].origin, Origin::Integration { .. }), "the integration document ranks next");
    assert!(documents[1].info.providers.is_some(), "the integration document keeps its providers");
}

#[tokio::test]
async fn keeps_only_policies_when_managed_policy_denies_the_integration() {
    let workspace = workspace();
    std::fs::write(
        workspace.system.join("opencode.json"),
        r#"{"policies":[{"action":"integration.use","resource":"opencode","effect":"deny"}]}"#,
    )
    .unwrap();
    let service = start(&workspace).await;

    let document = integration::make(
        json!({ "type": "credential", "id": "connection" }),
        None,
        integration::remote(&json!({
            "providers": { "acme": { "name": "Acme" } },
            "mcp": { "servers": { "remote": { "type": "remote", "url": "https://example.com/mcp" } } },
            "policies": [{ "action": "tool.use", "resource": "integrated", "effect": "allow" }]
        }))
        .unwrap(),
    );
    service.integration("opencode", Some(document)).await;

    let documents = documents(&service);
    let integrated = documents
        .iter()
        .find(|document| matches!(document.origin, Origin::Integration { .. }))
        .expect("the integration document is loaded");
    assert!(integrated.info.providers.is_none());
    assert!(integrated.info.mcp.is_none());
    assert!(integrated.info.websearch.is_none());
    assert_eq!(integrated.info.policies.as_ref().map(Vec::len), Some(1));
}

struct Workspace {
    _temp: TempDir,
    home: PathBuf,
    config: PathBuf,
    project: PathBuf,
    system: PathBuf,
}

fn workspace() -> Workspace {
    let temp = TempDir::new().unwrap();
    let root = opencode_util::path::realpath(temp.path()).unwrap();
    let project = root.join("project");
    let home = root.join("home");
    let config = root.join("global");
    let system = root.join("system");
    for directory in [&project, &home, &config, &system] {
        std::fs::create_dir_all(directory).unwrap();
    }
    Workspace { _temp: temp, home, config, project, system }
}

async fn start(workspace: &Workspace) -> ConfigService {
    let paths = GlobalPaths::from_vars(|name| match name {
        "HOME" => Some(workspace.home.clone().into_os_string()),
        "OPENCODE_CONFIG_DIR" => Some(workspace.config.clone().into_os_string()),
        _ => None,
    })
    .unwrap();
    let options = ConfigOptions {
        project: Some(true),
        global: Some(true),
        system: Some(SystemOption::Path(workspace.system.clone())),
        claude: Some(false),
        file: None,
        content: None,
    };
    let location = ConfigLocation { directory: workspace.project.clone(), project_id: "test".to_owned() };
    let deps = ServiceDeps {
        credentials: Arc::new(()),
        wellknown: Arc::new(WellKnown::new(Arc::new(NoKv))),
        channel: "local".to_owned(),
        flag_overrides: None,
    };
    ConfigService::start(options, location, paths, deps).await
}

fn documents(service: &ConfigService) -> Vec<Document> {
    service
        .entries()
        .into_iter()
        .filter_map(|entry| match entry {
            Entry::Document(document) => Some(document),
            Entry::Directory { .. } => None,
        })
        .collect()
}

fn to_value(info: &ConfigEntryDocumentInfo) -> Value {
    serde_json::from_str(&encode_json(info)).expect("a configuration encodes to JSON")
}
