use opencode_contract::generated::{
    ConfigEntryDocumentInfoPoliciesItem, ConfigEntryDocumentInfoPoliciesItemAction,
    ConfigEntryDocumentInfoPoliciesItemEffect,
};

use crate::entry::{Entry, Origin};
use crate::seams::CredentialValue;

pub type Policy = ConfigEntryDocumentInfoPoliciesItem;
pub type Action = ConfigEntryDocumentInfoPoliciesItemAction;
pub type Effect = ConfigEntryDocumentInfoPoliciesItemEffect;

#[derive(Clone, Debug, PartialEq)]
pub struct Statement {
    pub action: Action,
    pub resource: String,
    pub effect: Effect,
    pub message: String,
}

pub fn statements(entries: &[Entry]) -> Vec<Statement> {
    entries
        .iter()
        .rev()
        .filter_map(|entry| match entry {
            Entry::Document(document) => Some(document),
            Entry::Directory { .. } => None,
        })
        .flat_map(|document| {
            let message = message(&document.origin);
            document
                .info
                .policies
                .iter()
                .flatten()
                .map(|policy| Statement {
                    action: policy.action,
                    resource: policy.resource.clone(),
                    effect: policy.effect,
                    message: message.clone(),
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

pub fn matching<'a>(policies: &'a [Statement], action: Action, resource: &str) -> Option<&'a Statement> {
    policies
        .iter()
        .rev()
        .find(|statement| statement.action == action && crate::wildcard::matches(resource, &statement.resource, false))
}

pub fn decision(policies: &[Statement], action: Action, resource: &str) -> Effect {
    matching(policies, action, resource).map_or(Effect::Allow, |statement| statement.effect)
}

pub fn integration_resource(integration_id: &str, account: Option<&str>) -> String {
    match account.filter(|account| !account.is_empty()) {
        Some(account) => format!("{integration_id}#{account}"),
        None => integration_id.to_string(),
    }
}

pub fn account(value: &CredentialValue) -> Option<String> {
    ["orgID", "accountID"].into_iter().find_map(|key| {
        value
            .metadata
            .get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn message(origin: &Origin) -> String {
    match origin {
        Origin::Integration { organization, .. } => {
            organization.as_deref().filter(|organization| !organization.is_empty()).map_or_else(
                || "Blocked by your organization's policy".to_string(),
                |organization| format!("Blocked by {organization}'s policy"),
            )
        }
        _ => "Blocked by configuration policy".to_string(),
    }
}
