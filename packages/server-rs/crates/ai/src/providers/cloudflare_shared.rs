use opencode_api_types::js::encode_component;

use crate::protocols::shared::invalid_request;
use crate::providers::Settings;
use crate::route::endpoint::trim_base_url;
use crate::schema::AiError;

pub fn require_account_id(settings: &Settings, name: &str) -> Result<String, AiError> {
    match settings.string("accountId").filter(|value| !value.is_empty()) {
        Some(account_id) => Ok(account_id.to_string()),
        None => Err(invalid_request(format!("{name}.configure requires accountId unless baseURL is supplied"))),
    }
}

pub fn base_url(settings: &Settings, name: &str) -> Result<String, AiError> {
    if let Some(base_url) = settings.string("baseURL").filter(|value| !value.is_empty()) {
        return Ok(base_url.to_string());
    }
    let account_id = require_account_id(settings, name)?;
    Ok(format!("https://api.cloudflare.com/client/v4/accounts/{}/ai/v1", encode_component(&account_id)))
}

pub fn run_base_url(settings: &Settings, name: &str) -> Result<String, AiError> {
    let configured = base_url(settings, name)?;
    let url = trim_base_url(&configured);
    let Some(base) = url.strip_suffix("/ai/v1") else {
        return Err(invalid_request(format!(
            "{name} needs accountId or a baseURL ending in /ai/v1 to derive the Run API base"
        )));
    };
    Ok(format!("{base}/ai/run"))
}
