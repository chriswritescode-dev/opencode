use http::StatusCode;
use opencode_catalog::integration::IntegrationService;
use opencode_config::policy::{self, Effect};
use opencode_config::service::ConfigService;
use opencode_contract::codec::{JsonMap, encode_json};
use opencode_contract::generated::{IntegrationID, InvalidRequestError, InvalidRequestErrorTag};

use crate::response;
use crate::services::service_unavailable;

#[allow(clippy::result_large_err)]
pub async fn require_connectable(
    integration_id: &IntegrationID,
    integrations: &IntegrationService,
    config: &ConfigService,
) -> Result<(), response::Response> {
    let integration = match integrations.get(integration_id).await {
        Ok(integration) => integration,
        Err(error) => return Err(service_unavailable("integration", error)),
    };
    if is_mcp(integration.as_ref().and_then(|integration| integration.metadata.as_ref())) {
        return Ok(());
    }
    let statements = policy::statements(&config.entries());
    let denied = policy::matching(&statements, policy::Action::IntegrationConnect, &integration_id.0)
        .filter(|statement| statement.effect == Effect::Deny);
    let Some(denied) = denied else {
        return Ok(());
    };
    Err(response::bytes(
        StatusCode::BAD_REQUEST,
        "application/json",
        encode_json(&InvalidRequestError {
            tag: InvalidRequestErrorTag::InvalidRequestError,
            message: format!("{}: connecting {} is not allowed", denied.message, integration_id.0),
            kind: Some(Some("integration_policy".to_owned())),
            field: None,
        }),
    ))
}

fn is_mcp(metadata: Option<&JsonMap<serde_json::Value>>) -> bool {
    metadata
        .is_some_and(|metadata| metadata.0.iter().any(|(key, value)| key == "source" && value.as_str() == Some("mcp")))
}
