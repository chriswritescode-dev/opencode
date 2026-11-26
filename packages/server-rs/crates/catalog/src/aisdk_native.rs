use std::collections::BTreeSet;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub type JsonMap = Map<String, Value>;
pub type Headers = IndexMap<String, String>;

pub const AISDK_PREFIX: &str = "aisdk:";

pub fn is_aisdk(value: &str) -> bool {
    value.starts_with(AISDK_PREFIX)
}

pub fn aisdk(value: &str) -> String {
    if is_aisdk(value) {
        return value.to_string();
    }
    format!("{AISDK_PREFIX}{value}")
}

pub fn package_name(value: &str) -> String {
    if value.starts_with("@opencode-ai/ai/") {
        return value.replacen("@opencode-ai/", "@opencode/", 1);
    }
    match value.strip_prefix(AISDK_PREFIX) {
        Some(name) => name.to_string(),
        None => value.to_string(),
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Context {
    #[serde(rename = "providerID")]
    pub provider_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical: Option<String>,
    #[serde(rename = "modelID", default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<Shape>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<JsonMap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub specifier: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Shape {
    Responses,
    Completions,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Target {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<JsonMap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<Headers>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<JsonMap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variants: Option<Vec<Variant>>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Variant {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<JsonMap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<Headers>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<JsonMap>,
}

pub fn merge_overlay(base: Option<&JsonMap>, overlay: Option<&JsonMap>) -> Option<JsonMap> {
    match (base, overlay) {
        (None, None) => None,
        (None, Some(overlay)) => Some(overlay.clone()),
        (Some(base), None) => Some(base.clone()),
        (Some(base), Some(overlay)) => Some(merge_record(base, overlay)),
    }
}

fn merge_record(base: &JsonMap, overlay: &JsonMap) -> JsonMap {
    let mut merged = base.clone();
    for (key, right) in overlay {
        let value = match (merged.get(key), right) {
            (Some(Value::Object(left)), Value::Object(right)) => Value::Object(merge_record(left, right)),
            _ => right.clone(),
        };
        merged.insert(key.clone(), value);
    }
    merged
}

pub fn merge_headers(base: Option<&Headers>, overlay: Option<&Headers>) -> Option<Headers> {
    match (base, overlay) {
        (None, None) => None,
        (None, Some(overlay)) => Some(overlay.clone()),
        (Some(base), None) => Some(base.clone()),
        (Some(base), Some(overlay)) => {
            let mut merged: IndexMap<String, (String, String)> = IndexMap::new();
            for (name, value) in base.iter().chain(overlay.iter()) {
                merged.insert(name.to_lowercase(), (name.clone(), value.clone()));
            }
            Some(merged.into_iter().map(|(_, entry)| entry).collect())
        }
    }
}

const PACKAGES: &[(&str, &str)] = &[
    ("@ai-sdk/amazon-bedrock", "@opencode/ai/providers/amazon-bedrock"),
    ("@ai-sdk/alibaba", "@opencode/ai/providers/alibaba/chat"),
    ("@ai-sdk/anthropic", "@opencode/ai/providers/anthropic"),
    ("@ai-sdk/azure", "@opencode/ai/providers/azure/responses"),
    ("@ai-sdk/cerebras", "@opencode/ai/providers/cerebras"),
    ("@ai-sdk/cohere", "@opencode/ai/providers/cohere"),
    ("@ai-sdk/deepinfra", "@opencode/ai/providers/deepinfra"),
    ("@ai-sdk/google", "@opencode/ai/providers/google"),
    ("@ai-sdk/google-vertex", "@opencode/ai/providers/google-vertex"),
    ("@ai-sdk/google-vertex/anthropic", "@opencode/ai/providers/google-vertex/messages"),
    ("@ai-sdk/groq", "@opencode/ai/providers/groq"),
    ("@ai-sdk/mistral", "@opencode/ai/providers/mistral"),
    ("@ai-sdk/openai", "@opencode/ai/providers/openai"),
    ("@ai-sdk/openai-compatible", "@opencode/ai/providers/openai-compatible"),
    ("@ai-sdk/togetherai", "@opencode/ai/providers/togetherai"),
    ("@ai-sdk/xai", "@opencode/ai/providers/xai"),
    ("@ai-sdk/gateway", "@opencode/ai/providers/vercel-ai-gateway"),
    ("@openrouter/ai-sdk-provider", "@opencode/ai/providers/openrouter"),
    ("ai-gateway-provider", "@opencode/ai/providers/cloudflare-ai-gateway"),
    ("venice-ai-sdk-provider", "@opencode/ai/providers/venice"),
];

macro_rules! protocols {
    ($name:literal) => {
        &[
            ("@ai-sdk/openai-compatible", concat!("@opencode/ai/providers/", $name, "/chat")),
            ("@ai-sdk/anthropic", concat!("@opencode/ai/providers/", $name, "/messages")),
            ("@ai-sdk/openai", concat!("@opencode/ai/providers/", $name, "/responses")),
        ] as &[(&str, &str)]
    };
}

const HOSTS: &[(&str, &[(&str, &str)])] = &[
    ("alibaba", protocols!("alibaba")),
    ("alibaba-cn", protocols!("alibaba")),
    ("alibaba-coding-plan", protocols!("alibaba")),
    ("alibaba-coding-plan-cn", protocols!("alibaba")),
    ("alibaba-token-plan", protocols!("alibaba")),
    ("alibaba-token-plan-cn", protocols!("alibaba")),
    ("baseten", &[("@ai-sdk/openai-compatible", "@opencode/ai/providers/baseten")]),
    (
        "cloudflare-ai-gateway",
        &[
            ("@ai-sdk/anthropic", "@opencode/ai/providers/cloudflare-ai-gateway"),
            ("@ai-sdk/openai", "@opencode/ai/providers/cloudflare-ai-gateway"),
            ("@ai-sdk/openai-compatible", "@opencode/ai/providers/cloudflare-ai-gateway"),
            ("ai-gateway-provider", "@opencode/ai/providers/cloudflare-ai-gateway"),
        ],
    ),
    ("cohere", &[("@ai-sdk/openai-compatible", "@opencode/ai/providers/cohere/chat")]),
    ("cloudflare-workers-ai", &[("@ai-sdk/openai-compatible", "@opencode/ai/providers/cloudflare-workers-ai")]),
    ("deepseek", &[("@ai-sdk/openai-compatible", "@opencode/ai/providers/deepseek")]),
    ("digitalocean", &[("@ai-sdk/openai-compatible", "@opencode/ai/providers/digitalocean")]),
    ("fireworks-ai", &[("@ai-sdk/openai-compatible", "@opencode/ai/providers/fireworks")]),
    (
        "google-vertex",
        &[
            ("@ai-sdk/openai-compatible", "@opencode/ai/providers/google-vertex/chat"),
            ("@ai-sdk/mistral", "@opencode/ai/providers/google-vertex/mistral"),
        ],
    ),
    ("kimi-for-coding", protocols!("moonshot")),
    ("meta", protocols!("meta")),
    ("minimax", protocols!("minimax")),
    ("minimax-cn", protocols!("minimax")),
    ("minimax-coding-plan", protocols!("minimax")),
    ("minimax-cn-coding-plan", protocols!("minimax")),
    ("moonshotai", protocols!("moonshot")),
    ("moonshotai-cn", protocols!("moonshot")),
    ("zai", &[("@ai-sdk/openai-compatible", "@opencode/ai/providers/zai/chat")]),
    ("zai-coding-plan", protocols!("zai-coding-plan")),
    ("zhipuai", &[("@ai-sdk/openai-compatible", "@opencode/ai/providers/zai/chat")]),
    ("zhipuai-coding-plan", protocols!("zai-coding-plan")),
];

pub fn native(npm: &str, context: &Context) -> Option<String> {
    if let Some(host) = host(context.provider_id.as_str(), npm) {
        return Some(host.to_string());
    }
    if npm == "@ai-sdk/amazon-bedrock/mantle" {
        return Some(mantle(context.model_id.as_deref()));
    }
    if npm == "@ai-sdk/azure" && azure_chat(context) {
        return Some("@opencode/ai/providers/azure/chat".to_string());
    }
    package(npm).map(str::to_string)
}

fn azure_chat(context: &Context) -> bool {
    context.shape == Some(Shape::Completions)
        || context.settings.as_ref().and_then(|settings| settings.get("useCompletionUrls")).and_then(Value::as_bool)
            == Some(true)
}

fn mantle(model_id: Option<&str>) -> String {
    const BASE: &str = "@opencode/ai/providers/amazon-bedrock/mantle";
    match model_id {
        None => BASE.to_string(),
        Some(model_id) if model_id.contains("anthropic.") => format!("{BASE}/messages"),
        Some(model_id) => format!("{BASE}/{}", if model_id.contains("gpt-oss") { "chat" } else { "responses" }),
    }
}

const LEGACY_VERTEX: &[(&str, &str)] = &[
    ("@opencode/ai/providers/google-vertex-chat", "@opencode/ai/providers/google-vertex/chat"),
    ("@opencode/ai/providers/google-vertex-responses", "@opencode/ai/providers/google-vertex/responses"),
    ("@opencode/ai/providers/google-vertex-interactions", "@opencode/ai/providers/google-vertex/interactions"),
    ("@opencode/ai/providers/google-vertex-mistral", "@opencode/ai/providers/google-vertex/mistral"),
];

fn resolve(specifier: &str, context: &Context, builtins: &BTreeSet<&str>) -> Option<String> {
    let npm = package_name(specifier);
    if is_aisdk(specifier) || package(&npm).is_some() || host(context.provider_id.as_str(), &npm).is_some() {
        return native(&npm, context);
    }
    if npm == "@opencode/ai/providers/amazon-bedrock/mantle" || npm == "@opencode/ai/providers/amazon-bedrock-mantle" {
        return Some(mantle(context.model_id.as_deref()));
    }
    if let Some(legacy) = legacy_vertex(&npm) {
        return Some(legacy.to_string());
    }
    if (npm == "@opencode/ai/providers/azure" || npm == "@opencode/ai/providers/azure/responses") && azure_chat(context)
    {
        return Some("@opencode/ai/providers/azure/chat".to_string());
    }
    builtins.contains(npm.as_str()).then_some(npm)
}

pub fn rewrite(target: &mut Target, input: &Context, builtins: &BTreeSet<&str>) {
    let Some(specifier) = input.specifier.as_deref() else { return };
    let settings = decode_legacy(&target.settings.clone().unwrap_or_default());
    let mut context = input.clone();
    context.settings = Some(settings.clone());
    let Some(replacement) = resolve(specifier, &context, builtins) else { return };
    let translated = options(&replacement, context.model_id.as_deref(), &settings);
    if target.package.is_some() || replacement != specifier {
        target.package = Some(replacement.clone());
    }
    let identity = if replacement == "@opencode/ai/providers/openai-compatible" && !builtins.contains(specifier) {
        Some(input.canonical.clone().unwrap_or_else(|| input.provider_id.clone()))
    } else {
        None
    };
    target.settings = target.settings.as_ref().map(|_| {
        let mut next = translated.settings.clone();
        if let Some(provider) = identity {
            next.insert("provider".to_string(), Value::String(provider));
        }
        next
    });
    target.headers = merge_headers(translated.headers.as_ref(), target.headers.as_ref());
    target.body = merge_overlay(translated.body.as_ref(), target.body.as_ref());
    if let Some(variants) = target.variants.take() {
        let mapped = variants
            .into_iter()
            .map(|variant| {
                let overlay = options(
                    &replacement,
                    context.model_id.as_deref(),
                    &decode_legacy(&variant.settings.clone().unwrap_or_default()),
                );
                let headers = merge_headers(overlay.headers.as_ref(), variant.headers.as_ref());
                let body = merge_overlay(overlay.body.as_ref(), variant.body.as_ref());
                Variant {
                    id: variant.id,
                    settings: variant.settings.as_ref().map(|_| overlay.settings.clone()),
                    headers,
                    body,
                }
            })
            .collect();
        target.variants = Some(mapped);
    }
}

struct Overlay {
    settings: JsonMap,
    headers: Option<Headers>,
    body: Option<JsonMap>,
}

fn decode_legacy(settings: &JsonMap) -> JsonMap {
    let mut decoded = JsonMap::new();
    for (key, value) in settings {
        let field = match key.as_str() {
            "apiKey" | "baseURL" | "bearerToken" | "endpoint" | "region" | "accessKeyId" | "secretAccessKey"
            | "sessionToken" | "serviceTier" | "appName" | "appUrl" => value.is_string().then(|| value.clone()),
            "useCompletionUrls" => value.is_boolean().then(|| value.clone()),
            "headers" | "api_keys" => string_record(value),
            "extraBody" => value.is_object().then(|| value.clone()),
            "anthropicBeta" => string_array(value),
            "auth" => matches!(value.as_str(), Some("bearer" | "sigv4")).then(|| value.clone()),
            "credentials" => decode_credentials(value),
            "reasoningConfig" => decode_reasoning_config(value),
            "additionalModelRequestFields" => decode_additional_fields(value),
            _ => Some(value.clone()),
        };
        if let Some(field) = field {
            decoded.insert(key.clone(), field);
        }
    }
    decoded
}

fn string_record(value: &Value) -> Option<Value> {
    let entries = value.as_object()?;
    entries.values().all(Value::is_string).then(|| value.clone())
}

fn string_array(value: &Value) -> Option<Value> {
    let items = value.as_array()?;
    items.iter().all(Value::is_string).then(|| value.clone())
}

fn decode_credentials(value: &Value) -> Option<Value> {
    let entries = value.as_object()?;
    if !entries.get("accessKeyId").is_some_and(Value::is_string)
        || !entries.get("secretAccessKey").is_some_and(Value::is_string)
    {
        return None;
    }
    let mut credentials = JsonMap::new();
    for key in ["accessKeyId", "secretAccessKey", "sessionToken", "region"] {
        if let Some(field) = entries.get(key).filter(|field| field.is_string()) {
            credentials.insert(key.to_string(), field.clone());
        }
    }
    Some(Value::Object(credentials))
}

fn decode_reasoning_config(value: &Value) -> Option<Value> {
    let entries = value.as_object()?;
    let mut config = JsonMap::new();
    for key in ["type", "display", "maxReasoningEffort"] {
        if let Some(field) = entries.get(key).filter(|field| field.is_string()) {
            config.insert(key.to_string(), field.clone());
        }
    }
    if let Some(budget) = entries.get("budgetTokens").filter(|field| field.is_number()) {
        config.insert("budgetTokens".to_string(), budget.clone());
    }
    Some(Value::Object(config))
}

fn decode_additional_fields(value: &Value) -> Option<Value> {
    let entries = value.as_object()?;
    let mut fields = entries.clone();
    if let Some(betas) = fields.get("anthropic_beta")
        && !betas.as_array().is_some_and(|items| items.iter().all(Value::is_string))
    {
        fields.remove("anthropic_beta");
    }
    for key in ["output_config", "reasoning"] {
        if fields.get(key).is_some_and(|field| !field.is_object()) {
            fields.remove(key);
        }
    }
    Some(Value::Object(fields))
}

const OMIT_KEYS: &[&str] =
    &["headers", "extraBody", "useCompletionUrls", "appName", "appUrl", "api_keys", "compatibility"];

fn options(replacement: &str, model_id: Option<&str>, settings: &JsonMap) -> Overlay {
    let converse = replacement == "@opencode/ai/providers/amazon-bedrock" && model_id.is_some();
    let kept = omit(settings, OMIT_KEYS);
    let thinking = if converse { bedrock_thinking(model_id, settings) } else { None };
    let mut translated = if replacement.starts_with("@opencode/ai/providers/amazon-bedrock") {
        bedrock_settings(&kept, converse)
    } else {
        kept
    };
    if let Some(thinking) = thinking {
        translated.insert("thinking".to_string(), thinking);
    }
    let mut headers = settings_headers(settings);
    let mut body = object(settings, "extraBody").cloned();
    if converse && let Some(request) = bedrock_request(model_id, settings) {
        body = Some(request);
    }
    if replacement == "@opencode/ai/providers/openrouter"
        && let Some(request) = open_router_request(settings)
    {
        headers = Some(request);
    }
    Overlay { settings: translated, headers, body }
}

const BEDROCK_KEYS: &[&str] =
    &["bearerToken", "endpoint", "credentials", "credentialProvider", "accessKeyId", "secretAccessKey", "sessionToken"];

const CONVERSE_KEYS: &[&str] = &["additionalModelRequestFields", "reasoningConfig", "anthropicBeta", "serviceTier"];

fn bedrock_settings(settings: &JsonMap, converse: bool) -> JsonMap {
    let credentials = object(settings, "credentials").unwrap_or(settings);
    let region = text(settings, "region").or_else(|| text(credentials, "region"));
    let base_url = text(settings, "baseURL").or_else(|| text(settings, "endpoint"));
    let blocked: Vec<&str> =
        if converse { BEDROCK_KEYS.iter().chain(CONVERSE_KEYS).copied().collect() } else { BEDROCK_KEYS.to_vec() };
    let mut out = omit(settings, &blocked);
    if let Some(base_url) = base_url {
        let base_url = match region {
            Some(region) => base_url.replace("${AWS_REGION}", region),
            None => base_url.to_string(),
        };
        out.insert("baseURL".to_string(), Value::String(base_url));
    }
    if text(settings, "apiKey").is_none()
        && let Some(token) = text(settings, "bearerToken")
    {
        out.insert("apiKey".to_string(), Value::String(token.to_string()));
    }
    if let (Some(access_key), Some(secret_key)) =
        (text(credentials, "accessKeyId"), text(credentials, "secretAccessKey"))
    {
        let mut bundle = JsonMap::new();
        if let Some(region) = region {
            bundle.insert("region".to_string(), Value::String(region.to_string()));
        }
        bundle.insert("accessKeyId".to_string(), Value::String(access_key.to_string()));
        bundle.insert("secretAccessKey".to_string(), Value::String(secret_key.to_string()));
        if let Some(token) = text(credentials, "sessionToken") {
            bundle.insert("sessionToken".to_string(), Value::String(token.to_string()));
        }
        out.insert("credentials".to_string(), Value::Object(bundle));
    }
    out
}

fn bedrock_thinking(model_id: Option<&str>, settings: &JsonMap) -> Option<Value> {
    if !model_id.is_some_and(|model_id| model_id.contains("anthropic")) {
        return None;
    }
    let reasoning = object(settings, "reasoningConfig")?;
    if text(reasoning, "type") != Some("enabled") {
        return None;
    }
    let budget = reasoning.get("budgetTokens")?;
    let mut thinking = JsonMap::new();
    thinking.insert("type".to_string(), Value::String("enabled".to_string()));
    thinking.insert("budgetTokens".to_string(), budget.clone());
    Some(Value::Object(thinking))
}

fn bedrock_request(model_id: Option<&str>, settings: &JsonMap) -> Option<JsonMap> {
    let additional = object(settings, "additionalModelRequestFields").cloned().unwrap_or_default();
    let reasoning = object(settings, "reasoningConfig");
    let anthropic = model_id.is_some_and(|model_id| model_id.contains("anthropic"));
    let openai = model_id.is_some_and(|model_id| model_id.contains("openai."));
    let harmony = model_id.is_some_and(|model_id| model_id.contains("openai.gpt-oss"));
    let effort = reasoning.and_then(|reasoning| text(reasoning, "maxReasoningEffort"));
    let kind = reasoning.and_then(|reasoning| text(reasoning, "type"));
    let budget = reasoning.and_then(|reasoning| reasoning.get("budgetTokens"));
    let display = reasoning.and_then(|reasoning| text(reasoning, "display"));
    let betas = settings.get("anthropicBeta").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut overlay = JsonMap::new();
    if !betas.is_empty() {
        let mut merged = additional.get("anthropic_beta").and_then(Value::as_array).cloned().unwrap_or_default();
        merged.extend(betas);
        overlay.insert("anthropic_beta".to_string(), Value::Array(merged));
    }
    if anthropic && kind == Some("adaptive") {
        let mut thinking = JsonMap::new();
        thinking.insert("type".to_string(), Value::String("adaptive".to_string()));
        if let Some(display) = display {
            thinking.insert("display".to_string(), Value::String(display.to_string()));
        }
        overlay.insert("thinking".to_string(), Value::Object(thinking));
    }
    if let Some(effort) = effort {
        if anthropic {
            let mut output = object(&additional, "output_config").cloned().unwrap_or_default();
            output.insert("effort".to_string(), Value::String(effort.to_string()));
            overlay.insert("output_config".to_string(), Value::Object(output));
        } else if openai && harmony {
            overlay.insert("reasoning_effort".to_string(), Value::String(effort.to_string()));
        } else if openai {
            let mut reasoning = object(&additional, "reasoning").cloned().unwrap_or_default();
            reasoning.insert("effort".to_string(), Value::String(effort.to_string()));
            overlay.insert("reasoning".to_string(), Value::Object(reasoning));
        } else {
            let mut config = JsonMap::new();
            if kind.is_some_and(|kind| kind != "adaptive") {
                config.insert("type".to_string(), Value::String(kind.expect("a reasoning kind").to_string()));
            }
            if let Some(budget) = budget {
                config.insert("budgetTokens".to_string(), budget.clone());
            }
            config.insert("maxReasoningEffort".to_string(), Value::String(effort.to_string()));
            overlay.insert("reasoningConfig".to_string(), Value::Object(config));
        }
    }
    let fields = merge_record(&additional, &overlay);
    let mut body = JsonMap::new();
    if !fields.is_empty() {
        body.insert("additionalModelRequestFields".to_string(), Value::Object(fields));
    }
    if let Some(tier) = text(settings, "serviceTier") {
        let mut service_tier = JsonMap::new();
        service_tier.insert("type".to_string(), Value::String(tier.to_string()));
        body.insert("serviceTier".to_string(), Value::Object(service_tier));
    }
    if body.is_empty() { None } else { Some(body) }
}

fn open_router_request(settings: &JsonMap) -> Option<Headers> {
    let mut base = Headers::new();
    if let Some(app_name) = text(settings, "appName") {
        base.insert("X-OpenRouter-Title".to_string(), app_name.to_string());
    }
    if let Some(app_url) = text(settings, "appUrl") {
        base.insert("HTTP-Referer".to_string(), app_url.to_string());
    }
    if let Some(keys) = object(settings, "api_keys")
        && !keys.is_empty()
    {
        base.insert("X-Provider-API-Keys".to_string(), serde_json::to_string(keys).unwrap_or_default());
    }
    let overlay = settings_headers(settings);
    let headers = merge_headers(Some(&base), overlay.as_ref());
    match headers {
        Some(headers) if !headers.is_empty() => Some(headers),
        _ => None,
    }
}

fn omit(map: &JsonMap, keys: &[&str]) -> JsonMap {
    map.iter()
        .filter(|(key, _)| !keys.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn text<'a>(map: &'a JsonMap, key: &str) -> Option<&'a str> {
    map.get(key).and_then(Value::as_str)
}

fn object<'a>(map: &'a JsonMap, key: &str) -> Option<&'a JsonMap> {
    map.get(key).and_then(Value::as_object)
}

fn settings_headers(settings: &JsonMap) -> Option<Headers> {
    let headers = object(settings, "headers")?;
    headers.iter().map(|(name, value)| Some((name.clone(), value.as_str()?.to_string()))).collect()
}

fn package(npm: &str) -> Option<&'static str> {
    PACKAGES.iter().find(|(name, _)| *name == npm).map(|(_, value)| *value)
}

fn host(provider_id: &str, npm: &str) -> Option<&'static str> {
    let entries = HOSTS.iter().find(|(name, _)| *name == provider_id).map(|(_, entries)| *entries)?;
    entries.iter().find(|(name, _)| *name == npm).map(|(_, value)| *value)
}

fn legacy_vertex(npm: &str) -> Option<&'static str> {
    LEGACY_VERTEX.iter().find(|(name, _)| *name == npm).map(|(_, value)| *value)
}
