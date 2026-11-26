use opencode_contract::codec::{Decode, JsonMap, Path};
use opencode_contract::generated::{
    ConfigEntryDocumentInfo, ConfigEntryDocumentInfoMcp, ConfigEntryDocumentInfoMcpServersValue,
    ConfigEntryDocumentInfoPoliciesItem, ConfigEntryDocumentInfoPoliciesItemAction,
    ConfigEntryDocumentInfoPoliciesItemEffect, ConfigEntryDocumentInfoProvidersValue,
};
use serde_json::{Map, Value};

use crate::normalize::{Normalized, normalize};
use crate::service::IntegrationDocument;

const ALLOWED: [&str; 4] = ["providers", "mcp", "policies", "websearch"];
const CREDENTIALS: [&str; 3] = ["apiKey", "authToken", "accessToken"];

pub fn remote(body: &Value) -> Result<ConfigEntryDocumentInfo, String> {
    let Some(object) = body.as_object() else {
        return Err("the remote integration config is not an object".to_owned());
    };
    let mut allowed = Map::new();
    for key in ALLOWED {
        let Some(value) = object.get(key) else { continue };
        let value = if key == "policies" { known_policies(value)? } else { value.clone() };
        allowed.insert(key.to_owned(), value);
    }
    match normalize(&Value::Object(allowed)) {
        Normalized::Rejected { diagnostics } => {
            Err(format!("the remote integration config was rejected: {diagnostics:?}"))
        }
        Normalized::Normalized { encoded, .. } => {
            ConfigEntryDocumentInfo::decode(&Value::Object(encoded), &mut Path::new())
                .map_err(|error| error.to_string())
        }
    }
}

fn known_policies(value: &Value) -> Result<Value, String> {
    let Value::Array(statements) = value else {
        return Err("policies must be an array".to_owned());
    };
    let kept: Vec<Value> = statements
        .iter()
        .filter(|statement| recognized_action(statement) && recognized_effect(statement))
        .cloned()
        .collect();
    Vec::<ConfigEntryDocumentInfoPoliciesItem>::decode(&Value::Array(kept.clone()), &mut Path::new())
        .map_err(|error| error.to_string())?;
    Ok(Value::Array(kept))
}

fn recognized_action(statement: &Value) -> bool {
    statement.get("action").and_then(Value::as_str).is_none_or(|action| {
        ConfigEntryDocumentInfoPoliciesItemAction::decode(&Value::String(action.to_owned()), &mut Path::new()).is_ok()
    })
}

fn recognized_effect(statement: &Value) -> bool {
    statement.get("effect").and_then(Value::as_str).is_none_or(|effect| {
        ConfigEntryDocumentInfoPoliciesItemEffect::decode(&Value::String(effect.to_owned()), &mut Path::new()).is_ok()
    })
}

pub fn make(connection: Value, organization: Option<String>, remote: ConfigEntryDocumentInfo) -> IntegrationDocument {
    let info = ConfigEntryDocumentInfo {
        providers: remote.providers.map(|providers| {
            JsonMap(providers.0.into_iter().map(|(id, provider)| (id, clean_provider(provider))).collect())
        }),
        mcp: clean_mcp(remote.mcp),
        policies: remote.policies,
        websearch: remote.websearch,
        ..empty()
    };
    IntegrationDocument { connection, organization, info }
}

fn clean_provider(mut provider: ConfigEntryDocumentInfoProvidersValue) -> ConfigEntryDocumentInfoProvidersValue {
    provider.env = None;
    if let Some(settings) = provider.settings.as_mut() {
        without_credentials(&mut settings.rest);
    }
    if let Some(models) = provider.models.as_mut() {
        for (_, model) in models.0.iter_mut() {
            if let Some(settings) = model.settings.as_mut() {
                without_credentials(&mut settings.rest);
            }
            if let Some(variants) = model.variants.as_mut() {
                for variant in variants.iter_mut() {
                    if let Some(settings) = variant.settings.as_mut() {
                        without_credentials(&mut settings.rest);
                    }
                }
            }
        }
    }
    provider
}

fn clean_mcp(mcp: Option<ConfigEntryDocumentInfoMcp>) -> Option<ConfigEntryDocumentInfoMcp> {
    let servers = mcp?.servers?;
    let servers = servers
        .0
        .into_iter()
        .filter(|(_, server)| matches!(server, ConfigEntryDocumentInfoMcpServersValue::Remote(_)))
        .collect();
    Some(ConfigEntryDocumentInfoMcp { timeout: None, servers: Some(JsonMap(servers)) })
}

fn without_credentials(rest: &mut JsonMap<Option<Value>>) {
    rest.0.retain(|(key, _)| !CREDENTIALS.contains(&key.as_str()));
}

fn empty() -> ConfigEntryDocumentInfo {
    ConfigEntryDocumentInfo::decode(&Value::Object(Map::new()), &mut Path::new())
        .expect("an empty configuration decodes")
}
