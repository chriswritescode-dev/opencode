use super::ids::ProviderId;
use super::options::{
    CacheHint, CachePolicy, GenerationOptions, HttpOptions, ProviderMetadata, ProviderOptions, RequestTimeout,
    StringMap,
};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Message {
    System(SystemMessage),
    User(UserMessage),
    Assistant(AssistantMessage),
    Tool(ToolMessage),
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SystemMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub content: Vec<ContentPart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolChanges>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UserMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub content: Vec<ContentPart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_metadata: Option<ProviderMetadata>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub content: Vec<ContentPart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_metadata: Option<ProviderMetadata>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ToolMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub content: Vec<ContentPart>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ContentPart {
    Text(TextPart),
    Media(MediaPart),
    ToolCall(ToolCallPart),
    ToolResult(ToolResultPart),
    Reasoning(ReasoningPart),
    Compaction(CompactionPart),
    Effort(EffortPart),
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextPart {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<CacheHint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_metadata: Option<ProviderMetadata>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaPart {
    pub media: MediaAsset,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<CacheHint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_metadata: Option<ProviderMetadata>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallPart {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    pub input: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_executed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<CacheHint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_metadata: Option<ProviderMetadata>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultPart {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    pub result: ToolResultValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_executed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<CacheHint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_metadata: Option<ProviderMetadata>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningPart {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<CacheHint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_metadata: Option<ProviderMetadata>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionPart {
    pub provider: ProviderId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "deserialize_present_option")]
    pub text: Option<Option<String>>,
}

fn deserialize_present_option<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let text: Option<String> = serde::Deserialize::deserialize(deserializer)?;
    Ok(Some(text))
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EffortPart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ToolResultValue {
    Json { value: serde_json::Value },
    Text { value: serde_json::Value },
    Error { value: serde_json::Value },
    Content { value: Vec<ToolContent> },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ToolContent {
    Text {
        text: String,
    },
    File {
        uri: String,
        mime: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolOutput {
    pub structured: serde_json::Value,
    pub content: Vec<ToolContent>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ToolChanges {
    pub add: Vec<ToolEntry>,
    pub remove: Vec<String>,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ToolEntry {
    Tool(ToolDefinition),
    Namespace(ToolNamespace),
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freeform: Option<Freeform>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<CacheHint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Freeform {
    Text {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        field: Option<String>,
    },
    Regex {
        pattern: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        field: Option<String>,
    },
    Lark {
        grammar: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        field: Option<String>,
    },
}

pub fn freeform_field(schema: &serde_json::Value, freeform: Option<&Freeform>) -> Option<String> {
    let named = match freeform {
        Some(Freeform::Text { field }) => field.as_deref(),
        Some(Freeform::Regex { field, .. }) => field.as_deref(),
        Some(Freeform::Lark { field, .. }) => field.as_deref(),
        None => None,
    };
    let properties = schema.get("properties").and_then(serde_json::Value::as_object);
    if named.is_none() && properties.map_or(0, serde_json::Map::len) != 1 {
        return None;
    }
    let properties = properties?;
    let (name, property) = match named {
        Some(named) => properties.get_key_value(named)?,
        None => properties.iter().next()?,
    };
    if property.get("type").and_then(serde_json::Value::as_str) != Some("string") {
        return None;
    }
    let required = schema.get("required")?.as_array()?;
    required.iter().any(|value| value.as_str() == Some(name.as_str())).then(|| name.clone())
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolNamespace {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub tools: Vec<ToolEntry>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolChoice {
    pub r#type: ToolChoiceType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disable_parallel_tool_use: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolChoiceType {
    Auto,
    None,
    Required,
    Tool,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum SystemPart {
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache: Option<CacheHint>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<serde_json::Map<String, serde_json::Value>>,
    },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaAsset {
    pub source: MediaSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info: Option<MediaInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_metadata: Option<ProviderMetadata>,
    #[serde(skip)]
    pub headers: Option<StringMap>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum MediaSource {
    #[serde(rename_all = "camelCase")]
    Bytes { data: String, media_type: String },
    #[serde(rename_all = "camelCase")]
    Base64 { data: String, media_type: String },
    #[serde(rename_all = "camelCase")]
    Url {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires_at: Option<serde_json::Number>,
    },
    #[serde(rename_all = "camelCase")]
    Ref {
        provider: ProviderId,
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media_type: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MediaInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LlmRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub system: Vec<SystemPart>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(flatten)]
    pub generation: GenerationOptions,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<CachePolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_raw: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_options: Option<ProviderOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<StringMap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<RequestTimeout>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<HttpOptions>,
}
