use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{ConnectionInfo, CredentialValue};
use serde_json::Value;

use crate::integration;

pub const DEFAULT_SERVER: &str = "https://opencode.ai/console";

const RETIRED_SERVER: &str = "https://console.opencode.ai";

pub fn server(value: &CredentialValue) -> String {
    let server = metadata_field(value, "server").and_then(Value::as_str).unwrap_or(DEFAULT_SERVER);
    rewrite_retired(server)
}

pub fn headers(value: &CredentialValue) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    if let Some(token) = token(value) {
        headers.push(("authorization".to_owned(), format!("Bearer {token}")));
    }
    if let Some(org_id) = metadata_field(value, "orgID").and_then(Value::as_str) {
        headers.push(("x-org-id".to_owned(), org_id.to_owned()));
    }
    headers
}

pub fn normalize_server(input: &str) -> Result<String, String> {
    let url = url::Url::parse(input).map_err(|error| format!("Invalid OpenCode server URL: {error}"))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err("Invalid OpenCode server URL: expected HTTP(S)".to_owned());
    }
    Ok(format!("{}{}", url.origin().ascii_serialization(), url.path().trim_end_matches('/')))
}

pub fn workspace_key(connection: &ConnectionInfo, value: &CredentialValue) -> String {
    let organization = metadata_field(value, "orgID").cloned().unwrap_or(Value::Null);
    let key = serde_json::json!([server(value), integration::key(connection), organization]);
    serde_json::to_string(&key).expect("a workspace key serializes")
}

pub(crate) fn token(value: &CredentialValue) -> Option<&str> {
    match value {
        CredentialValue::Oauth(value) => Some(&value.access),
        CredentialValue::Key(value) => Some(&value.key),
        CredentialValue::External(_) => None,
    }
}

pub fn metadata_field<'a>(value: &'a CredentialValue, key: &str) -> Option<&'a Value> {
    let metadata: Option<&JsonMap<Value>> = match value {
        CredentialValue::Oauth(value) => value.metadata.as_ref(),
        CredentialValue::Key(value) => value.metadata.as_ref(),
        CredentialValue::External(value) => value.metadata.as_ref(),
    };
    metadata?.0.iter().find(|(name, _)| name == key).map(|(_, value)| value)
}

fn rewrite_retired(server: &str) -> String {
    let Some(rest) = server.strip_prefix(RETIRED_SERVER) else {
        return server.to_owned();
    };
    if !rest.is_empty() && !rest.starts_with('/') {
        return server.to_owned();
    }
    format!("{DEFAULT_SERVER}{rest}")
}

#[cfg(test)]
mod tests {
    use opencode_contract::codec::JsonMap;
    use opencode_contract::generated::{
        CredentialKey, CredentialKeyType, CredentialOAuth, CredentialOAuthType, CredentialValue, IntegrationMethodID,
    };
    use serde_json::{Value, json};

    use super::{DEFAULT_SERVER, headers, normalize_server, server};

    fn oauth(metadata: Option<JsonMap<Value>>) -> CredentialValue {
        CredentialValue::Oauth(CredentialOAuth {
            r#type: CredentialOAuthType::Oauth,
            method_id: IntegrationMethodID("device".to_owned()),
            refresh: "refresh".to_owned(),
            access: "access".to_owned(),
            expires: 1.0,
            metadata,
        })
    }

    fn key(metadata: Option<JsonMap<Value>>) -> CredentialValue {
        CredentialValue::Key(CredentialKey { r#type: CredentialKeyType::Key, key: "secret".to_owned(), metadata })
    }

    fn with_server(server: Value) -> CredentialValue {
        oauth(Some(JsonMap(vec![("server".to_owned(), server)])))
    }

    #[test]
    fn rewrites_the_retired_console_host() {
        assert_eq!(server(&with_server(json!("https://console.opencode.ai"))), DEFAULT_SERVER);
        assert_eq!(
            server(&with_server(json!("https://console.opencode.ai/workspace"))),
            format!("{DEFAULT_SERVER}/workspace")
        );
        assert_eq!(
            server(&with_server(json!("https://console.opencode.ai.evil.example"))),
            "https://console.opencode.ai.evil.example"
        );
        assert_eq!(server(&with_server(json!("https://dev.opencode.ai/console"))), "https://dev.opencode.ai/console");
    }

    #[test]
    fn falls_back_to_the_default_server() {
        assert_eq!(server(&oauth(None)), DEFAULT_SERVER);
        assert_eq!(server(&with_server(json!(42))), DEFAULT_SERVER);
        assert_eq!(server(&with_server(json!(""))), "");
    }

    #[test]
    fn authenticates_as_the_member_in_their_workspace() {
        let value = oauth(Some(JsonMap(vec![("orgID".to_owned(), json!("wrk_anomaly"))])));
        assert_eq!(
            headers(&value),
            vec![
                ("authorization".to_owned(), "Bearer access".to_owned()),
                ("x-org-id".to_owned(), "wrk_anomaly".to_owned()),
            ]
        );
        let value = key(Some(JsonMap(vec![("orgID".to_owned(), json!(7))])));
        assert_eq!(headers(&value), vec![("authorization".to_owned(), "Bearer secret".to_owned())]);
    }

    #[test]
    fn normalizes_an_http_server_to_its_origin_and_path() {
        assert_eq!(
            normalize_server("https://opencode.ai/console/").expect("a valid server"),
            "https://opencode.ai/console"
        );
        assert_eq!(
            normalize_server("https://opencode.ai/console/index.html?query=1#fragment").expect("a valid server"),
            "https://opencode.ai/console/index.html"
        );
        assert_eq!(normalize_server("https://opencode.ai").expect("a valid server"), "https://opencode.ai");
        assert_eq!(
            normalize_server("ftp://opencode.ai").expect_err("a non-HTTP server"),
            "Invalid OpenCode server URL: expected HTTP(S)"
        );
    }
}
