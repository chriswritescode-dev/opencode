use std::collections::{BTreeSet, HashMap};

use serde_json::{Map, Value};

pub(crate) const AISDK_PREFIX: &str = "aisdk:";

pub(crate) fn is_aisdk(value: &str) -> bool {
    value.starts_with(AISDK_PREFIX)
}

pub(crate) fn package_name(value: Option<&str>) -> Option<String> {
    let value = value?;
    if value.starts_with("@opencode-ai/ai/") {
        return Some(value.replacen("@opencode-ai/", "@opencode/", 1));
    }
    Some(value.strip_prefix(AISDK_PREFIX).unwrap_or(value).to_string())
}

pub(crate) fn merge_overlay(
    base: Option<&Map<String, Value>>,
    overlay: Option<&Map<String, Value>>,
) -> Option<Map<String, Value>> {
    match (base, overlay) {
        (None, None) => None,
        (Some(base), None) => Some(base.clone()),
        (None, Some(overlay)) => Some(overlay.clone()),
        (Some(base), Some(overlay)) => {
            let mut merged = base.clone();
            merge_record(&mut merged, overlay);
            Some(merged)
        }
    }
}

fn merge_record(target: &mut Map<String, Value>, overlay: &Map<String, Value>) {
    for (key, right) in overlay {
        let merged = match target.get(key) {
            Some(left) if left.is_object() && right.is_object() => {
                let mut merged = left.as_object().cloned().unwrap_or_default();
                merge_record(&mut merged, right.as_object().expect("checked object"));
                Value::Object(merged)
            }
            _ => right.clone(),
        };
        target.insert(key.clone(), merged);
    }
}

pub(crate) fn merge_headers(
    base: Option<&[(String, String)]>,
    overlay: Option<&[(String, String)]>,
) -> Option<Vec<(String, String)>> {
    if base.is_none() && overlay.is_none() {
        return None;
    }
    let mut order: Vec<(String, String)> = Vec::new();
    let mut positions: HashMap<String, usize> = HashMap::new();
    for (key, value) in base.unwrap_or_default().iter().chain(overlay.unwrap_or_default().iter()) {
        let lower = key.to_lowercase();
        match positions.get(&lower) {
            Some(position) => order[*position] = (key.clone(), value.clone()),
            None => {
                positions.insert(lower, order.len());
                order.push((key.clone(), value.clone()));
            }
        }
    }
    Some(order)
}

#[allow(dead_code)]
#[derive(Clone, Copy)]
pub(crate) enum Shape {
    Responses,
    Completions,
}

pub(crate) struct Context<'a> {
    pub provider_id: &'a str,
    pub model_id: Option<&'a str>,
    pub shape: Option<Shape>,
    pub settings: Option<&'a Map<String, Value>>,
    pub builtins: &'a BTreeSet<&'static str>,
}

pub(crate) struct Translation {
    pub settings: Map<String, Value>,
    pub headers: Option<Vec<(String, String)>>,
    pub body: Option<Map<String, Value>>,
}

pub(crate) fn resolve(specifier: &str, context: &Context<'_>) -> Option<String> {
    let npm = package_name(Some(specifier))?;
    if is_aisdk(specifier) || package(&npm).is_some() || host(&npm, context.provider_id).is_some() {
        return native(&npm, context);
    }
    if npm == "@opencode/ai/providers/amazon-bedrock/mantle" || npm == "@opencode/ai/providers/amazon-bedrock-mantle" {
        return Some(mantle(context.model_id));
    }
    if let Some(replacement) = legacy_vertex(&npm) {
        return Some(replacement);
    }
    if (npm == "@opencode/ai/providers/azure" || npm == "@opencode/ai/providers/azure/responses") && azure_chat(context)
    {
        return Some("@opencode/ai/providers/azure/chat".to_string());
    }
    context.builtins.contains(npm.as_str()).then_some(npm)
}

pub(crate) fn native(npm: &str, context: &Context<'_>) -> Option<String> {
    if let Some(host) = host(npm, context.provider_id) {
        return Some(host);
    }
    if npm == "@ai-sdk/amazon-bedrock/mantle" {
        return Some(mantle(context.model_id));
    }
    if npm == "@ai-sdk/azure" && azure_chat(context) {
        return Some("@opencode/ai/providers/azure/chat".to_string());
    }
    package(npm).map(str::to_string)
}

fn azure_chat(context: &Context<'_>) -> bool {
    matches!(context.shape, Some(Shape::Completions))
        || context
            .settings
            .and_then(|settings| settings.get("useCompletionUrls"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

fn mantle(model_id: Option<&str>) -> String {
    let Some(model_id) = model_id else {
        return "@opencode/ai/providers/amazon-bedrock/mantle".to_string();
    };
    if model_id.contains("anthropic.") {
        return "@opencode/ai/providers/amazon-bedrock/mantle/messages".to_string();
    }
    let shape = if model_id.contains("gpt-oss") { "chat" } else { "responses" };
    format!("@opencode/ai/providers/amazon-bedrock/mantle/{shape}")
}

fn legacy_vertex(npm: &str) -> Option<String> {
    Some(
        match npm {
            "@opencode/ai/providers/google-vertex-chat" => "@opencode/ai/providers/google-vertex/chat",
            "@opencode/ai/providers/google-vertex-responses" => "@opencode/ai/providers/google-vertex/responses",
            "@opencode/ai/providers/google-vertex-interactions" => "@opencode/ai/providers/google-vertex/interactions",
            "@opencode/ai/providers/google-vertex-mistral" => "@opencode/ai/providers/google-vertex/mistral",
            _ => return None,
        }
        .to_string(),
    )
}

fn package(npm: &str) -> Option<&'static str> {
    Some(match npm {
        "@ai-sdk/amazon-bedrock" => "@opencode/ai/providers/amazon-bedrock",
        "@ai-sdk/alibaba" => "@opencode/ai/providers/alibaba/chat",
        "@ai-sdk/anthropic" => "@opencode/ai/providers/anthropic",
        "@ai-sdk/azure" => "@opencode/ai/providers/azure/responses",
        "@ai-sdk/cerebras" => "@opencode/ai/providers/cerebras",
        "@ai-sdk/cohere" => "@opencode/ai/providers/cohere",
        "@ai-sdk/deepinfra" => "@opencode/ai/providers/deepinfra",
        "@ai-sdk/google" => "@opencode/ai/providers/google",
        "@ai-sdk/google-vertex" => "@opencode/ai/providers/google-vertex",
        "@ai-sdk/google-vertex/anthropic" => "@opencode/ai/providers/google-vertex/messages",
        "@ai-sdk/groq" => "@opencode/ai/providers/groq",
        "@ai-sdk/mistral" => "@opencode/ai/providers/mistral",
        "@ai-sdk/openai" => "@opencode/ai/providers/openai",
        "@ai-sdk/openai-compatible" => "@opencode/ai/providers/openai-compatible",
        "@ai-sdk/togetherai" => "@opencode/ai/providers/togetherai",
        "@ai-sdk/xai" => "@opencode/ai/providers/xai",
        "@ai-sdk/gateway" => "@opencode/ai/providers/vercel-ai-gateway",
        "@openrouter/ai-sdk-provider" => "@opencode/ai/providers/openrouter",
        "ai-gateway-provider" => "@opencode/ai/providers/cloudflare-ai-gateway",
        "venice-ai-sdk-provider" => "@opencode/ai/providers/venice",
        _ => return None,
    })
}

fn protocols(name: &str, npm: &str) -> Option<String> {
    Some(match npm {
        "@ai-sdk/openai-compatible" => format!("@opencode/ai/providers/{name}/chat"),
        "@ai-sdk/anthropic" => format!("@opencode/ai/providers/{name}/messages"),
        "@ai-sdk/openai" => format!("@opencode/ai/providers/{name}/responses"),
        _ => return None,
    })
}

fn host(npm: &str, provider_id: &str) -> Option<String> {
    let exact = match provider_id {
        "baseten" => (npm == "@ai-sdk/openai-compatible").then_some("@opencode/ai/providers/baseten"),
        "cloudflare-ai-gateway" => {
            matches!(npm, "@ai-sdk/anthropic" | "@ai-sdk/openai" | "@ai-sdk/openai-compatible" | "ai-gateway-provider")
                .then_some("@opencode/ai/providers/cloudflare-ai-gateway")
        }
        "cohere" => (npm == "@ai-sdk/openai-compatible").then_some("@opencode/ai/providers/cohere/chat"),
        "cloudflare-workers-ai" => {
            (npm == "@ai-sdk/openai-compatible").then_some("@opencode/ai/providers/cloudflare-workers-ai")
        }
        "deepseek" => (npm == "@ai-sdk/openai-compatible").then_some("@opencode/ai/providers/deepseek"),
        "digitalocean" => (npm == "@ai-sdk/openai-compatible").then_some("@opencode/ai/providers/digitalocean"),
        "fireworks-ai" => (npm == "@ai-sdk/openai-compatible").then_some("@opencode/ai/providers/fireworks"),
        "google-vertex" => match npm {
            "@ai-sdk/openai-compatible" => Some("@opencode/ai/providers/google-vertex/chat"),
            "@ai-sdk/mistral" => Some("@opencode/ai/providers/google-vertex/mistral"),
            _ => None,
        },
        "zai" => (npm == "@ai-sdk/openai-compatible").then_some("@opencode/ai/providers/zai/chat"),
        "zhipuai" => (npm == "@ai-sdk/openai-compatible").then_some("@opencode/ai/providers/zai/chat"),
        _ => None,
    };
    if let Some(replacement) = exact {
        return Some(replacement.to_string());
    }
    match provider_id {
        "alibaba"
        | "alibaba-cn"
        | "alibaba-coding-plan"
        | "alibaba-coding-plan-cn"
        | "alibaba-token-plan"
        | "alibaba-token-plan-cn" => protocols("alibaba", npm),
        "kimi-for-coding" => protocols("moonshot", npm),
        "meta" => protocols("meta", npm),
        "minimax" | "minimax-cn" | "minimax-coding-plan" | "minimax-cn-coding-plan" => protocols("minimax", npm),
        "moonshotai" | "moonshotai-cn" => protocols("moonshot", npm),
        "zai-coding-plan" | "zhipuai-coding-plan" => protocols("zai-coding-plan", npm),
        _ => None,
    }
}

pub(crate) fn options(settings: &Map<String, Value>) -> Translation {
    let mut kept = settings.clone();
    for key in ["headers", "extraBody", "useCompletionUrls", "appName", "appUrl", "api_keys", "compatibility"] {
        kept.remove(key);
    }
    let headers = settings
        .get("headers")
        .and_then(Value::as_object)
        .map(|headers| {
            headers
                .iter()
                .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.to_string())))
                .collect::<Vec<_>>()
        })
        .filter(|headers: &Vec<(String, String)>| !headers.is_empty());
    let body = settings.get("extraBody").and_then(Value::as_object).cloned();
    Translation { settings: kept, headers, body }
}
