use std::collections::BTreeMap;

pub type ProviderOptions = serde_json::Map<String, serde_json::Value>;
pub type ProviderMetadata = serde_json::Map<String, serde_json::Value>;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GenerationOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheHint {
    pub r#type: CacheHintType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheHintType {
    Ephemeral,
    Persistent,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum CachePolicy {
    Mode(CacheMode),
    Object(CachePolicyObject),
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheMode {
    Auto,
    None,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CachePolicyObject {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages: Option<CacheMessages>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum CacheMessages {
    Named(String),
    Tail { tail: serde_json::Number },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RequestEncoding {
    Zstd,
    Gzip,
    None,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientTls {
    pub cert: String,
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passphrase: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct HttpOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding: Option<RequestEncoding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<ClientTls>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum HttpTimeout {
    Millis(serde_json::Number),
    Disabled(bool),
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TimeoutOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<HttpTimeout>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<HttpTimeout>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk: Option<HttpTimeout>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum RequestTimeout {
    Timeout(HttpTimeout),
    Config(TimeoutOptions),
}
