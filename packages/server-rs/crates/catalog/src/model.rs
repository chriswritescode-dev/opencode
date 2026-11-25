use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use crate::aisdk_native::{Context, merge_headers, merge_overlay, options, resolve};

const PROVIDER_ONLY_SETTINGS: [&str; 5] = ["timeout", "headerTimeout", "chunkTimeout", "transport", "tls"];

#[derive(Clone, Debug)]
pub(crate) struct ModelState {
    pub id: String,
    pub model_id: String,
    pub r#type: Option<String>,
    pub family: Option<String>,
    pub name: String,
    pub compatibility: Option<Value>,
    pub package: Option<String>,
    pub settings: Option<Map<String, Value>>,
    pub headers: Option<Vec<(String, String)>>,
    pub body: Option<Map<String, Value>>,
    pub capabilities: Value,
    pub variants: Vec<Value>,
    pub released: f64,
    pub cost: Vec<Value>,
    pub enabled: bool,
    pub limit: Value,
}

impl ModelState {
    pub(crate) fn new(id: &str) -> Self {
        Self {
            id: id.to_string(),
            model_id: id.to_string(),
            r#type: None,
            family: None,
            name: id.to_string(),
            compatibility: None,
            package: None,
            settings: None,
            headers: None,
            body: None,
            capabilities: default_capabilities(),
            variants: Vec::new(),
            released: 0.0,
            cost: Vec::new(),
            enabled: true,
            limit: json!({ "context": 200_000, "output": 32_000 }),
        }
    }
}

pub(crate) fn default_capabilities() -> Value {
    json!({ "tools": true, "input": ["text", "image"], "output": ["text"] })
}

pub(crate) fn model_settings(settings: Option<&Map<String, Value>>) -> Option<Map<String, Value>> {
    let mut settings = settings?.clone();
    for key in PROVIDER_ONLY_SETTINGS {
        settings.remove(key);
    }
    Some(settings)
}

pub(crate) fn merge_objects(base: Option<&Value>, overlay: &Map<String, Value>) -> Value {
    let mut merged = base.and_then(Value::as_object).cloned().unwrap_or_default();
    for (key, value) in overlay {
        merged.insert(key.clone(), value.clone());
    }
    Value::Object(merged)
}

pub(crate) struct Overlay<'a> {
    pub provider_id: &'a str,
    pub provider_package: Option<&'a str>,
    pub provider_canonical: Option<&'a str>,
    pub id: &'a str,
    pub config: &'a Map<String, Value>,
    pub base: Option<&'a ModelState>,
    pub existing: Option<&'a ModelState>,
    pub inherit: bool,
    pub builtins: &'a BTreeSet<&'static str>,
}

pub(crate) fn overlay_model(input: Overlay<'_>) -> ModelState {
    let start = if input.inherit { input.base } else { input.existing };
    let mut model = start.cloned().unwrap_or_else(|| ModelState::new(input.id));
    model.id = input.id.to_string();
    let config = input.config;
    if input.inherit && config.contains_key("package") {
        model.package = None;
    }
    if let Some(value) = config.get("type").and_then(Value::as_str) {
        model.r#type = Some(value.to_string());
    }
    if let Some(value) = config.get("family").and_then(Value::as_str) {
        model.family = Some(value.to_string());
    }
    if let Some(value) = config.get("name").and_then(Value::as_str)
        && !value.is_empty()
    {
        model.name = value.to_string();
    }
    if let Some(value) = config.get("modelID").and_then(Value::as_str) {
        model.model_id = value.to_string();
    }
    if let Some(value) = config.get("releaseDate").and_then(Value::as_str)
        && let Some(released) = parse_release(value)
    {
        model.released = released;
    }
    if let Some(value) = config.get("compatibility").and_then(Value::as_object) {
        model.compatibility = Some(merge_objects(model.compatibility.as_ref(), value));
    }
    if let Some(value) = config.get("package").and_then(Value::as_str) {
        model.package = Some(value.to_string());
    }
    if let Some(value) = config.get("settings").and_then(Value::as_object) {
        model.settings = merge_overlay(model.settings.as_ref(), Some(value));
    }
    if let Some(value) = config.get("headers").and_then(Value::as_object) {
        model.headers = merge_headers(model.headers.as_deref(), Some(&pairs(value)));
    }
    if let Some(value) = config.get("body").and_then(Value::as_object) {
        model.body = merge_overlay(model.body.as_ref(), Some(value));
    }
    let capabilities = config.get("capabilities").and_then(Value::as_object);
    if let Some(capabilities) = capabilities {
        model.capabilities = merge_capabilities(capabilities, &model.capabilities);
    }
    if let Some(value) = config.get("limit").and_then(Value::as_object) {
        model.limit = merge_objects(Some(&model.limit), value);
    }
    rewrite_model(&mut model, input.provider_id, input.provider_package, input.provider_canonical, input.builtins);
    if let Some(reasoning) = capabilities.and_then(|capabilities| capabilities.get("reasoning")) {
        model.variants = resolve_variants(&model, input.provider_package, &supports(reasoning));
    }
    if let Some(variants) = config.get("variants").and_then(Value::as_array) {
        overlay_variants(&mut model.variants, variants);
    }
    if let Some(cost) = config.get("cost") {
        model.cost = convert_cost(cost);
    }
    if let Some(disabled) = config.get("disabled").and_then(Value::as_bool) {
        model.enabled = !disabled;
    }
    let had_variants = input.existing.is_some_and(|existing| !existing.variants.is_empty());
    let repackaged =
        input.base.is_some_and(|base| !base.variants.is_empty() && model.package.as_deref() != base.package.as_deref());
    if !config.contains_key("variants")
        && capabilities.and_then(|capabilities| capabilities.get("reasoning")).is_none()
        && ((input.base.is_none() && !had_variants) || repackaged)
    {
        model.variants = resolve_variants(&model, input.provider_package, &[Support::Effort { values: None }]);
    }
    rewrite_model(&mut model, input.provider_id, input.provider_package, input.provider_canonical, input.builtins);
    model
}

fn rewrite_model(
    model: &mut ModelState,
    provider_id: &str,
    provider_package: Option<&str>,
    provider_canonical: Option<&str>,
    builtins: &BTreeSet<&'static str>,
) {
    let Some(specifier) = model.package.as_deref().or(provider_package).map(str::to_string) else {
        return;
    };
    let settings = model.settings.clone().unwrap_or_default();
    let context =
        Context { provider_id, model_id: Some(&model.model_id), shape: None, settings: Some(&settings), builtins };
    let Some(replacement) = resolve(&specifier, &context) else {
        return;
    };
    let translated = options(&settings);
    if model.package.is_some() || replacement != specifier {
        model.package = Some(replacement.clone());
    }
    let identity = (replacement == "@opencode/ai/providers/openai-compatible"
        && !builtins.contains(specifier.as_str()))
    .then(|| provider_canonical.unwrap_or(provider_id).to_string());
    if model.settings.is_some() {
        let mut merged = translated.settings;
        if let Some(identity) = identity {
            merged.insert("provider".to_string(), Value::String(identity));
        }
        model.settings = Some(merged);
    }
    model.headers = merge_headers(translated.headers.as_deref(), model.headers.as_deref());
    model.body = merge_overlay(translated.body.as_ref(), model.body.as_ref());
}

fn merge_capabilities(config: &Map<String, Value>, base: &Value) -> Value {
    let default = default_capabilities();
    let fallback = if base.is_object() { base } else { &default };
    let tools = config
        .get("tools")
        .and_then(Value::as_bool)
        .or_else(|| fallback.get("tools").and_then(Value::as_bool))
        .unwrap_or(true);
    let freeform = config.get("freeform").or_else(|| fallback.get("freeform")).cloned();
    let input =
        config.get("input").or_else(|| fallback.get("input")).cloned().unwrap_or_else(|| json!(["text", "image"]));
    let output = config.get("output").or_else(|| fallback.get("output")).cloned().unwrap_or_else(|| json!(["text"]));
    let temperature = config.get("temperature").or_else(|| fallback.get("temperature")).cloned();
    let mut capabilities = Map::new();
    capabilities.insert("tools".to_string(), Value::Bool(tools));
    if let Some(freeform) = freeform {
        capabilities.insert("freeform".to_string(), freeform);
    }
    capabilities.insert("input".to_string(), input);
    capabilities.insert("output".to_string(), output);
    if let Some(temperature) = temperature {
        capabilities.insert("temperature".to_string(), temperature);
    }
    Value::Object(capabilities)
}

fn pairs(value: &Map<String, Value>) -> Vec<(String, String)> {
    value.iter().filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.to_string()))).collect()
}

fn overlay_variants(variants: &mut Vec<Value>, overlay: &[Value]) {
    for item in overlay {
        let Some(config) = item.as_object() else {
            continue;
        };
        let Some(id) = config.get("id").and_then(Value::as_str) else {
            continue;
        };
        let position = variants.iter().position(|variant| variant.get("id").and_then(Value::as_str) == Some(id));
        let mut variant = position.map(|position| variants[position].clone()).unwrap_or_else(|| json!({ "id": id }));
        if let Some(settings) = config.get("settings").and_then(Value::as_object) {
            let merged = merge_overlay(variant.get("settings").and_then(Value::as_object), Some(settings));
            if let Some(merged) = merged {
                variant["settings"] = Value::Object(merged);
            }
        }
        if let Some(headers) = config.get("headers").and_then(Value::as_object) {
            let existing = variant.get("headers").and_then(Value::as_object).map(pairs).unwrap_or_default();
            let merged = merge_headers(Some(&existing), Some(&pairs(headers)));
            if let Some(merged) = merged {
                variant["headers"] =
                    Value::Object(merged.into_iter().map(|(key, value)| (key, Value::String(value))).collect());
            }
        }
        if let Some(body) = config.get("body").and_then(Value::as_object) {
            let merged = merge_overlay(variant.get("body").and_then(Value::as_object), Some(body));
            if let Some(merged) = merged {
                variant["body"] = Value::Object(merged);
            }
        }
        match position {
            Some(position) => variants[position] = variant,
            None => variants.push(variant),
        }
    }
}

fn convert_cost(cost: &Value) -> Vec<Value> {
    let entries = match cost {
        Value::Array(entries) => entries.clone(),
        Value::Object(_) => vec![cost.clone()],
        _ => Vec::new(),
    };
    entries
        .into_iter()
        .filter_map(|entry| {
            let entry = entry.as_object()?;
            let mut converted = Map::new();
            if let Some(tier) = entry.get("tier").filter(|tier| !tier.is_null()) {
                converted.insert("tier".to_string(), tier.clone());
            }
            converted.insert("input".to_string(), entry.get("input").cloned().unwrap_or_else(|| json!(0)));
            converted.insert("output".to_string(), entry.get("output").cloned().unwrap_or_else(|| json!(0)));
            let cache = entry.get("cache").and_then(Value::as_object);
            converted.insert(
                "cache".to_string(),
                json!({
                    "read": cache.and_then(|cache| cache.get("read")).cloned().unwrap_or_else(|| json!(0)),
                    "write": cache.and_then(|cache| cache.get("write")).cloned().unwrap_or_else(|| json!(0)),
                }),
            );
            Some(Value::Object(converted))
        })
        .collect()
}

pub(crate) fn parse_release(value: &str) -> Option<f64> {
    let mut parts = value.split('-');
    let year = parts.next()?.parse::<i64>().ok()?;
    let month = parts.next()?.parse::<i64>().ok()?;
    let day = parts.next().map_or(Some(1), |day| day.parse::<i64>().ok())?;
    Some(days_from_civil(year, month, day) as f64 * 86_400_000.0)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

pub(crate) enum Support {
    Effort { values: Option<Vec<String>> },
}

pub(crate) fn supports(reasoning: &Value) -> Vec<Support> {
    if reasoning.get("supported").and_then(Value::as_bool) != Some(true) {
        return Vec::new();
    }
    match reasoning.get("effort").and_then(Value::as_array) {
        Some(values) => vec![Support::Effort {
            values: Some(
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|value| *value != "default")
                    .map(str::to_string)
                    .collect(),
            ),
        }],
        None => Vec::new(),
    }
}

const EFFORTS: [&str; 3] = ["low", "medium", "high"];
const ANTHROPIC_OUTPUT_TOKEN_MAX: f64 = 32_000.0;

fn resolve_variants(model: &ModelState, provider_package: Option<&str>, supports: &[Support]) -> Vec<Value> {
    let package = model.package.as_deref().or(provider_package);
    let Some(Support::Effort { values }) = supports.first() else {
        return Vec::new();
    };
    match package {
        Some("@opencode/ai/providers/openai-compatible") => openai_chat(values.as_deref()),
        Some("@opencode/ai/providers/anthropic") => {
            anthropic_messages(&model.model_id, &model.limit, values.as_deref())
        }
        _ => Vec::new(),
    }
}

fn efforts(values: &[String], spell: impl Fn(&str) -> Value) -> Vec<Value> {
    values.iter().map(|effort| json!({ "id": effort, "settings": spell(effort) })).collect()
}

fn openai_chat(values: Option<&[String]>) -> Vec<Value> {
    let defaults = EFFORTS.map(str::to_string);
    let values = values.unwrap_or(&defaults);
    efforts(values, |effort| json!({ "reasoningEffort": effort }))
}

fn anthropic_messages(model_id: &str, limit: &Value, values: Option<&[String]>) -> Vec<Value> {
    let info = claude_info(model_id);
    let opus45 = info.family == Some("opus") && info.major == 4 && info.minor == 5;
    let output = limit.get("output").and_then(Value::as_f64).unwrap_or(32_000.0);
    if info.manual && !opus45 {
        return budget_variants(
            output,
            1024.0,
            ANTHROPIC_OUTPUT_TOKEN_MAX,
            |tokens| json!({ "thinking": { "type": "enabled", "budgetTokens": tokens } }),
        );
    }
    let thinking = if opus45 {
        manual_thinking(output)
    } else {
        Some(json!({ "thinking": { "type": "adaptive", "display": "summarized" } }))
    };
    let Some(thinking) = thinking else {
        return Vec::new();
    };
    let defaults: Vec<String> = if info.major == 4 && info.minor == 6 {
        ["low", "medium", "high", "max"].iter().map(|value| value.to_string()).collect()
    } else {
        ["low", "medium", "high", "xhigh", "max"].iter().map(|value| value.to_string()).collect()
    };
    let values = values.unwrap_or(&defaults);
    values
        .iter()
        .filter(|effort| !info.always || *effort != "none")
        .map(|effort| {
            let settings = if effort == "none" {
                json!({ "thinking": { "type": "disabled" } })
            } else {
                let mut settings = thinking.as_object().cloned().unwrap_or_default();
                settings.insert("effort".to_string(), Value::String(effort.to_string()));
                Value::Object(settings)
            };
            json!({ "id": effort, "settings": settings })
        })
        .collect()
}

fn manual_thinking(output: f64) -> Option<Value> {
    let tokens = 16_000.0_f64.min(output - 1.0);
    if tokens < 1024.0 {
        return None;
    }
    Some(json!({ "thinking": { "type": "enabled", "budgetTokens": tokens } }))
}

fn budget_variants(output: f64, minimum: f64, ceiling: f64, spell: impl Fn(f64) -> Value) -> Vec<Value> {
    let maximum = (ceiling - 1.0).min(output - 1.0);
    if maximum <= 0.0 {
        return Vec::new();
    }
    let high = minimum.max(((maximum + 1.0) / 2.0).floor()).min(maximum);
    vec![json!({ "id": "high", "settings": spell(high) }), json!({ "id": "max", "settings": spell(maximum) })]
}

struct ClaudeInfo {
    family: Option<&'static str>,
    major: i32,
    minor: i32,
    manual: bool,
    always: bool,
}

fn claude_info(model_id: &str) -> ClaudeInfo {
    let lower = model_id.to_lowercase();
    let bytes = lower.as_bytes();
    let families = ["opus", "sonnet", "haiku", "fable", "mythos"];
    let mut family = None;
    let mut major = -1;
    let mut minor = 0;
    'families: for candidate in families {
        let mut offset = 0;
        while let Some(relative) = lower[offset..].find(candidate) {
            let start = offset + relative;
            let mut index = start + candidate.len();
            if index < bytes.len() && bytes[index] == b'-' {
                index += 1;
                if let Some((value, next)) = read_number(bytes, index) {
                    family = Some(candidate);
                    major = value;
                    if next < bytes.len()
                        && (bytes[next] == b'.' || bytes[next] == b'-')
                        && let Some((value, _)) = read_number(bytes, next + 1)
                    {
                        minor = value;
                    }
                    break 'families;
                }
            }
            offset = start + candidate.len();
        }
    }
    if family.is_none()
        && let Some(version) = lower.find("claude-")
    {
        let mut index = version + "claude-".len();
        if let Some((value, next)) = read_number(bytes, index) {
            major = value;
            index = next;
            minor = 0;
            if index < bytes.len()
                && (bytes[index] == b'.' || bytes[index] == b'-')
                && let Some((value, next)) = read_number(bytes, index + 1)
            {
                minor = value;
                index = next;
            }
            if index < bytes.len() && bytes[index] == b'-' {
                let rest = &lower[index + 1..];
                family = families.into_iter().find(|candidate| rest.starts_with(candidate));
            }
        }
    }
    let manual = (major == 3 && minor == 7) || (major == 4 && minor < 6);
    let always = matches!(family, Some("fable") | Some("mythos")) || lower.contains("mythos-preview");
    ClaudeInfo { family, major, minor, manual, always }
}

fn read_number(bytes: &[u8], start: usize) -> Option<(i32, usize)> {
    let mut end = start;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    if end == start {
        return None;
    }
    std::str::from_utf8(&bytes[start..end]).ok()?.parse().ok().map(|value| (value, end))
}
