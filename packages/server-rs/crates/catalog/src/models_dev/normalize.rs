use std::collections::BTreeSet;
use std::hash::Hash;
use std::sync::LazyLock;

use indexmap::IndexMap;
use serde_json::{Map, Value};

use opencode_ai::ProviderPackages;
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    ModelCapabilities, ModelCompatibility, ModelCost, ModelCostCache, ModelCostTier, ModelCostTierType, ModelInfo,
    ModelInfoLimit, ModelInfoStatus, ModelInfoTime, ModelMaxTokensField, ModelMediaInput, ModelReasoningField,
    ModelReasoningFieldString, ModelReasoningFieldString2, ModelReasoningFieldString3, ModelSettings, ModelType,
    ModelTypeString, ModelTypeString2, ModelTypeString3, ModelTypeString4, ModelVariant, ModelVariantSettings,
    MoneyUSDPerMillionTokens, ProviderID, ProviderInfo, ProviderInfoActivation, ProviderSettings, ToolFreeformFormat,
};
use opencode_util::time::{Date, parse_rfc3339};

use super::{Snapshot, VoiceModel, source, v1};
use crate::aisdk_native;
use crate::aisdk_native::Context;
use crate::variant;

const TYPES: [&str; 5] = ["chat", "decision", "image", "video", "realtime"];

const BUILDS_OWN_URL: [&str; 15] = [
    "@opencode/ai/providers/amazon-bedrock",
    "@opencode/ai/providers/azure/chat",
    "@opencode/ai/providers/azure/images",
    "@opencode/ai/providers/azure/messages",
    "@opencode/ai/providers/azure/responses",
    "@opencode/ai/providers/cloudflare-ai-gateway",
    "@opencode/ai/providers/cloudflare-ai-gateway/decisions",
    "@opencode/ai/providers/cloudflare-workers-ai",
    "@opencode/ai/providers/cloudflare-workers-ai/decisions",
    "@opencode/ai/providers/cloudflare-workers-ai/images",
    "@opencode/ai/providers/google-vertex/gemini",
    "@opencode/ai/providers/google-vertex/images",
    "@opencode/ai/providers/google-vertex/messages",
    "aisdk:@jerome-benoit/sap-ai-provider-v2",
    "aisdk:watsonx-ai-provider",
];

const COMPATIBLE: [&str; 2] =
    ["@opencode/ai/providers/openai-compatible", "@opencode/ai/providers/anthropic-compatible"];

static BUILTIN: LazyLock<BTreeSet<&'static str>> = LazyLock::new(|| ProviderPackages::builtin().specifiers());

struct Route {
    package: String,
    freeform: Option<Vec<ToolFreeformFormat>>,
    base_url: Option<String>,
    compatibility: Option<ModelCompatibility>,
}

struct Source {
    model: source::Model,
    route: Route,
}

struct ProviderDefaults<'a> {
    id: &'a str,
    package: &'a str,
    base_url: Option<&'a str>,
}

#[derive(Default)]
struct ModelInfoInput<'a> {
    name: Option<&'a str>,
    cost: Option<&'a [ModelCost]>,
    request: Option<&'a source::Mode>,
    variants: Option<&'a [ModelVariant]>,
}

pub fn normalize(catalog: &Map<String, Value>, recommended: bool) -> Vec<Snapshot> {
    let converted = v1::is_v1(catalog).then(|| v1::to_v2(catalog));
    let source_catalog = converted.as_ref().unwrap_or(catalog);
    let mut failed: Vec<(String, String)> = Vec::new();
    let mut unknown = BTreeSet::new();
    let mut snapshots = Vec::new();
    for value in source_catalog.values() {
        if let Some(snapshot) = provider_snapshot(value, recommended, &mut failed, &mut unknown) {
            snapshots.push(snapshot);
        }
    }
    if !failed.is_empty() {
        let models = failed.iter().take(10).map(|(model, _)| model.as_str()).collect::<Vec<_>>().join(", ");
        log::warn!(
            "Skipped {} models.dev models that failed to parse: {} (first issue: {})",
            failed.len(),
            models,
            failed[0].1
        );
    }
    if !unknown.is_empty() {
        log::debug!(
            "Skipped models.dev APIs with entrypoints this build doesn't have: {}",
            unknown.iter().cloned().collect::<Vec<_>>().join(", ")
        );
    }
    snapshots
}

fn provider_snapshot(
    value: &Value,
    recommended: bool,
    failed: &mut Vec<(String, String)>,
    unknown: &mut BTreeSet<String>,
) -> Option<Snapshot> {
    let provider = source::decode_provider(value)?;
    let provider_id = provider.id.clone();
    let prefer = recommended && provider_id != "google-vertex";
    let mut voices = Vec::new();
    let mut sources = Vec::new();
    for (key, value) in &provider.models {
        let model = match source::decode_model(value) {
            Ok(model) => model,
            Err(error) => {
                failed.push((format!("{}/{}", provider.id, key), error.to_string()));
                continue;
            }
        };
        let api = if model.kind == "realtime" { model.api.keys().next().cloned() } else { None };
        let speaks = model.modalities.output.iter().any(|output| output.starts_with("audio"));
        if let Some(api) = api
            && speaks
            && model.capabilities.tools.supported
            && model.status.as_deref() != Some("deprecated")
        {
            voices.push(VoiceModel {
                id: model.id.clone(),
                name: model.name.clone(),
                api,
                release_date: model.release_date.clone(),
            });
        }
        if !TYPES.contains(&model.kind.as_str()) {
            continue;
        }
        let Some(route) = route(&provider_id, &model, unknown, prefer) else {
            continue;
        };
        sources.push(Source { model, route });
    }
    if sources.is_empty() {
        return None;
    }
    let package_name = most_common(sources.iter().map(|source| source.route.package.clone()))?;
    let base_url = if sources.iter().all(|source| source.route.base_url.is_some()) {
        most_common(sources.iter().filter_map(|source| source.route.base_url.clone()))
    } else {
        None
    };
    let info = ProviderInfo {
        id: ProviderID(provider_id.clone()),
        canonical: None,
        integration_id: None,
        name: provider.name.clone(),
        activation: ProviderInfoActivation::Auto,
        package: package_name.clone(),
        compatibility: None,
        settings: base_url.as_ref().filter(|url| !url.is_empty()).map(|url| ProviderSettings {
            timeout: None,
            header_timeout: None,
            chunk_timeout: None,
            compaction: None,
            cache: None,
            transport: None,
            encoding: None,
            tls: None,
            rest: JsonMap(vec![("baseURL".to_string(), Value::String(url.clone()))]),
        }),
        headers: None,
        body: None,
    };
    let defaults = ProviderDefaults { id: &provider_id, package: &package_name, base_url: base_url.as_deref() };
    let models = sources
        .iter()
        .flat_map(|source| {
            let base_cost = cost(source.model.cost.as_ref());
            let base = model_info(
                &defaults,
                &source.model.id,
                source,
                ModelInfoInput { cost: Some(&base_cost), ..Default::default() },
            );
            let variants = variant::resolve(
                &variant::Model {
                    id: source.model.id.clone(),
                    model_id: source.model.id.clone(),
                    provider_id: provider_id.clone(),
                    package: Some(source.route.package.clone()),
                    output: source.model.limit.as_ref().and_then(|limit| limit.output).unwrap_or(0.0) as i64,
                },
                &variant::supports(&reasoning(&source.model.capabilities.reasoning)),
            );
            let variants = variants.iter().map(contract_variant).collect::<Vec<_>>();
            let mut base = base;
            base.variants = variants.clone();
            let mut models = vec![base];
            let modes = entries(
                source.model.experimental.as_ref().and_then(|experimental| experimental.modes.as_ref()),
                source::decode_mode,
            );
            for (mode, options) in modes {
                let name = format!("{} {}", source.model.name, capitalize(&mode));
                let mode_cost = mode_cost(&source.model, &base_cost, &mode, &options);
                let id = format!("{}-{}", source.model.id, mode);
                models.push(model_info(
                    &defaults,
                    &id,
                    source,
                    ModelInfoInput {
                        name: Some(&name),
                        cost: Some(&mode_cost),
                        request: Some(&options),
                        variants: Some(&variants),
                    },
                ));
            }
            models
        })
        .collect();
    Some(Snapshot { info, models, voices: (!voices.is_empty()).then_some(voices), environment: provider.env.clone() })
}

fn capitalize(value: &str) -> String {
    let mut characters = value.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}

fn route(provider_id: &str, model: &source::Model, unknown: &mut BTreeSet<String>, recommended: bool) -> Option<Route> {
    let apis = model
        .api
        .iter()
        .filter_map(|(protocol, value)| {
            let entry = source::decode_api(value)?;
            let opencodeai = loadable(entry.sdk.as_ref().and_then(|sdk| sdk.opencodeai.as_deref()), unknown);
            Some((protocol.as_str(), entry, opencodeai))
        })
        .collect::<Vec<_>>();
    let eligible = |(_, entry, opencodeai): &(&str, source::Api, Option<String>)| {
        if model.kind == "chat" {
            return opencodeai.is_some() || entry.sdk.as_ref().and_then(|sdk| sdk.aisdk.as_ref()).is_some();
        }
        opencodeai.as_deref().is_some_and(|value| value.ends_with(&format!("/{}s", model.kind)))
    };
    let preferred = recommended
        .then(|| model.recommended_api.as_ref().and_then(Value::as_str))
        .flatten()
        .and_then(|wanted| apis.iter().find(|api| api.0 == wanted && eligible(api)));
    let (protocol, api, opencodeai) = preferred.or_else(|| apis.iter().find(|api| eligible(api)))?;
    let package = match opencodeai {
        Some(opencodeai) => opencodeai.clone(),
        None => {
            let aisdk = api.sdk.as_ref().and_then(|sdk| sdk.aisdk.as_deref())?;
            aisdk_native::native(
                aisdk,
                &Context {
                    provider_id: provider_id.to_string(),
                    model_id: Some(model.id.clone()),
                    shape: match *protocol {
                        "responses" => Some(aisdk_native::Shape::Responses),
                        "chat-completions" => Some(aisdk_native::Shape::Completions),
                        _ => None,
                    },
                    ..Default::default()
                },
            )
            .unwrap_or_else(|| aisdk_native::aisdk(aisdk))
        }
    };
    if package.is_empty() {
        return None;
    }
    let compatibility = api.compatibility.as_ref().map(compatibility);
    let compatibility = compatibility.filter(|value| !is_empty_compatibility(value));
    Some(Route {
        freeform: api
            .capabilities
            .as_ref()
            .and_then(|capabilities| capabilities.tools.as_ref())
            .and_then(|tools| tools.freeform.as_ref().map(|values| freeform(values))),
        base_url: api
            .base_url
            .as_ref()
            .filter(|url| !url.is_empty() && !BUILDS_OWN_URL.contains(&package.as_str()))
            .cloned(),
        package,
        compatibility,
    })
}

fn loadable(value: Option<&str>, unknown: &mut BTreeSet<String>) -> Option<String> {
    let value = value?;
    let name = aisdk_native::package_name(value);
    if !name.starts_with("@opencode/ai/") || BUILTIN.contains(name.as_str()) {
        return Some(value.to_string());
    }
    unknown.insert(name);
    None
}

fn compatibility(value: &source::Compatibility) -> ModelCompatibility {
    ModelCompatibility {
        reasoning_field: value
            .reasoning_field
            .as_ref()
            .filter(|field| !field.is_empty())
            .map(|field| reasoning_field(field)),
        require_reasoning: None,
        max_tokens_field: match value.max_tokens_field.as_deref() {
            Some("max_completion_tokens") => Some(ModelMaxTokensField::MaxCompletionTokens),
            Some("max_tokens") => Some(ModelMaxTokensField::MaxTokens),
            _ => None,
        },
        require_finish_reason: None,
        require_assistant_after_tool: None,
        supports_prompt_cache_key: None,
        stream_usage: None,
        cache_format: None,
        supports_thinking_block_binding: None,
        supports_effort_updates: None,
        media_input: match value.media_input.as_deref() {
            Some("assets") => Some(ModelMediaInput::Assets),
            Some("state") => Some(ModelMediaInput::State),
            Some("root") => Some(ModelMediaInput::Root),
            _ => None,
        },
    }
}

fn reasoning_field(value: &str) -> ModelReasoningField {
    match value {
        "reasoning" => ModelReasoningField::String(ModelReasoningFieldString::Reasoning),
        "reasoning_content" => ModelReasoningField::String2(ModelReasoningFieldString2::ReasoningContent),
        "reasoning_text" => ModelReasoningField::String3(ModelReasoningFieldString3::ReasoningText),
        other => ModelReasoningField::String4(other.to_string()),
    }
}

fn is_empty_compatibility(value: &ModelCompatibility) -> bool {
    value.reasoning_field.is_none() && value.max_tokens_field.is_none() && value.media_input.is_none()
}

fn freeform(values: &[String]) -> Vec<ToolFreeformFormat> {
    values
        .iter()
        .filter_map(|value| match value.as_str() {
            "text" => Some(ToolFreeformFormat::Text),
            "regex" => Some(ToolFreeformFormat::Regex),
            "lark" => Some(ToolFreeformFormat::Lark),
            _ => None,
        })
        .collect()
}

fn most_common<T: Clone + Eq + Hash>(values: impl IntoIterator<Item = T>) -> Option<T> {
    let mut counts: IndexMap<T, usize> = IndexMap::new();
    for value in values {
        *counts.entry(value).or_insert(0) += 1;
    }
    counts.into_iter().reduce(|best, entry| if entry.1 > best.1 { entry } else { best }).map(|(value, _)| value)
}

fn reasoning(value: &source::Reasoning) -> variant::Reasoning {
    variant::Reasoning {
        supported: value.supported,
        toggle: value.toggle.unwrap_or(false),
        effort: value.effort.clone(),
        budget: value.budget.as_ref().map(|budget| variant::Budget { min: budget.min, max: budget.max }),
    }
}

fn contract_variant(value: &variant::Variant) -> ModelVariant {
    ModelVariant {
        id: value.id.clone(),
        settings: value.settings.as_ref().map(|settings| ModelVariantSettings {
            compaction: None,
            encoding: None,
            rest: JsonMap(settings.iter().map(|(key, value)| (key.clone(), value.clone())).collect()),
        }),
        headers: None,
        body: value
            .body
            .as_ref()
            .map(|body| JsonMap(body.iter().map(|(key, value)| (key.clone(), value.clone())).collect())),
    }
}

fn model_info(defaults: &ProviderDefaults<'_>, id: &str, source: &Source, input: ModelInfoInput<'_>) -> ModelInfo {
    let model = &source.model;
    let mut settings = Vec::new();
    if let Some(url) = source.route.base_url.as_deref()
        && Some(url) != defaults.base_url
    {
        settings.push(("baseURL".to_string(), Value::String(url.to_string())));
    }
    if COMPATIBLE.contains(&source.route.package.as_str()) {
        settings.push(("provider".to_string(), Value::String(defaults.id.to_string())));
    }
    let limit = model.limit.as_ref();
    ModelInfo {
        id: id.to_string(),
        model_id: model.id.clone(),
        provider_id: ProviderID(defaults.id.to_string()),
        canonical: None,
        r#type: Some(model_type(&model.kind)),
        family: model.family.clone(),
        name: input.name.unwrap_or(&model.name).to_string(),
        compatibility: source.route.compatibility.clone(),
        package: (source.route.package != defaults.package).then(|| source.route.package.clone()),
        settings: (!settings.is_empty()).then_some(ModelSettings {
            compaction: None,
            cache: None,
            encoding: None,
            rest: JsonMap(settings),
        }),
        headers: input
            .request
            .and_then(|mode| mode.headers.as_ref())
            .map(|headers| JsonMap(headers.iter().map(|(key, value)| (key.clone(), value.clone())).collect())),
        body: input
            .request
            .and_then(|mode| mode.body.as_ref())
            .map(|body| JsonMap(body.iter().map(|(key, value)| (key.clone(), value.clone())).collect())),
        capabilities: ModelCapabilities {
            tools: model.capabilities.tools.supported,
            freeform: source.route.freeform.clone(),
            input: modalities(&model.modalities.input),
            output: modalities(&model.modalities.output),
            temperature: model.capabilities.temperature,
        },
        variants: input.variants.unwrap_or(&[]).to_vec(),
        time: ModelInfoTime { released: released(&model.release_date) },
        cost: input.cost.map(<[ModelCost]>::to_vec).unwrap_or_else(|| cost(model.cost.as_ref())),
        status: status(model.status.as_deref()),
        enabled: true,
        limit: ModelInfoLimit {
            context: limit.and_then(|limit| limit.context).unwrap_or(0.0),
            input: limit.and_then(|limit| limit.input),
            output: limit.and_then(|limit| limit.output).unwrap_or(0.0),
        },
    }
}

fn model_type(kind: &str) -> ModelType {
    match kind {
        "chat" => ModelType::String(ModelTypeString::Chat),
        "decision" => ModelType::String2(ModelTypeString2::Decision),
        "image" => ModelType::String3(ModelTypeString3::Image),
        "video" => ModelType::String4(ModelTypeString4::Video),
        other => ModelType::String5(other.to_string()),
    }
}

fn status(value: Option<&str>) -> ModelInfoStatus {
    match value {
        Some("alpha") => ModelInfoStatus::Alpha,
        Some("beta") => ModelInfoStatus::Beta,
        Some("deprecated") => ModelInfoStatus::Deprecated,
        _ => ModelInfoStatus::Active,
    }
}

fn modality(value: &str) -> &str {
    if value == "application/pdf" {
        return "pdf";
    }
    let kind = value.split('/').next().unwrap_or(value);
    if matches!(kind, "text" | "image" | "audio" | "video") {
        return kind;
    }
    value
}

fn modalities(values: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    values
        .iter()
        .map(|value| modality(value))
        .filter(|value| seen.insert(value.to_string()))
        .map(str::to_string)
        .collect()
}

fn released(date: &str) -> f64 {
    if let Some(millis) = parse_rfc3339(date) {
        return millis as f64;
    }
    if let Some(day) = Date::parse(date) {
        return (day.days() * 86_400_000) as f64;
    }
    if let Some(month) = year_month(date) {
        return (month.days() * 86_400_000) as f64;
    }
    if let Some(year) = year(date) {
        return (year.days() * 86_400_000) as f64;
    }
    0.0
}

fn year_month(text: &str) -> Option<Date> {
    let bytes = text.as_bytes();
    if bytes.len() != 7 || bytes[4] != b'-' {
        return None;
    }
    Date::new(text[0..4].parse().ok()?, text[5..7].parse().ok()?, 1)
}

fn year(text: &str) -> Option<Date> {
    if text.len() != 4 {
        return None;
    }
    Date::new(text.parse().ok()?, 1, 1)
}

fn entries<T>(record: Option<&Map<String, Value>>, decode: impl Fn(&Value) -> Option<T>) -> Vec<(String, T)> {
    record
        .map(|record| {
            record.iter().filter_map(|(key, value)| decode(value).map(|entry| (key.clone(), entry))).collect()
        })
        .unwrap_or_default()
}

fn money(value: f64) -> MoneyUSDPerMillionTokens {
    MoneyUSDPerMillionTokens(value)
}

fn zero_cost() -> ModelCost {
    ModelCost {
        tier: None,
        input: money(0.0),
        output: money(0.0),
        cache: ModelCostCache { read: money(0.0), write: money(0.0) },
    }
}

fn cache_write(cache: Option<&source::CachePrices>) -> Option<f64> {
    let first = |write: Option<&source::CacheWrite>| match write {
        Some(source::CacheWrite::Price(price)) => Some(*price),
        Some(source::CacheWrite::Record(record)) => record.values().next().copied(),
        None => None,
    };
    first(cache.and_then(|cache| cache.write.as_ref()))
        .or_else(|| first(cache.and_then(|cache| cache.explicit.as_ref()?.write.as_ref())))
}

fn cost(input: Option<&source::Cost>) -> Vec<ModelCost> {
    let Some(text) = input.and_then(|cost| cost.text.as_ref()) else {
        return vec![zero_cost()];
    };
    let (Some(text_input), Some(text_output)) = (text.input.as_ref(), text.output.as_ref()) else {
        return vec![zero_cost()];
    };
    let per_token = |unit: Option<&str>| unit.is_none_or(|unit| unit == "token");
    if !per_token(text_input.unit.as_deref()) || !per_token(text_output.unit.as_deref()) {
        return vec![zero_cost()];
    }
    let base = ModelCost {
        tier: None,
        input: money(text_input.price),
        output: money(text_output.price),
        cache: ModelCostCache {
            read: money(text_input.cache.as_ref().and_then(|cache| cache.read).unwrap_or(0.0)),
            write: money(cache_write(text_input.cache.as_ref()).unwrap_or(0.0)),
        },
    };
    let mut tiers: IndexMap<u64, ModelCost> = IndexMap::new();
    for entry in text_input.overrides.iter().flatten() {
        let Some(key) = tier_entry(&mut tiers, &base, &entry.when) else { continue };
        let current = tiers.get(&key).cloned().unwrap_or_else(zero_cost);
        tiers.insert(
            key,
            ModelCost {
                input: entry.price.map(money).unwrap_or(current.input),
                cache: ModelCostCache {
                    read: entry.cache.as_ref().and_then(|cache| cache.read).map(money).unwrap_or(current.cache.read),
                    write: cache_write(entry.cache.as_ref()).map(money).unwrap_or(current.cache.write),
                },
                ..current
            },
        );
    }
    for entry in text_output.overrides.iter().flatten() {
        let Some(key) = tier_entry(&mut tiers, &base, &entry.when) else { continue };
        let current = tiers.get(&key).cloned().unwrap_or_else(zero_cost);
        tiers.insert(key, ModelCost { output: entry.price.map(money).unwrap_or(current.output), ..current });
    }
    let mut result = vec![base];
    result.extend(tiers.into_values());
    result
}

fn tier_entry(tiers: &mut IndexMap<u64, ModelCost>, base: &ModelCost, when: &Map<String, Value>) -> Option<u64> {
    let context = if when.len() == 1 { when.get("context").and_then(context_condition_size) } else { None };
    let size = context?;
    let key = size.to_bits();
    tiers.entry(key).or_insert_with(|| ModelCost {
        tier: Some(ModelCostTier { r#type: ModelCostTierType::Context, size }),
        ..base.clone()
    });
    Some(key)
}

fn context_condition_size(value: &Value) -> Option<f64> {
    let object = value.as_object()?;
    object.get("gt").and_then(Value::as_f64).or_else(|| object.get("gte").and_then(Value::as_f64))
}

fn mode_cost(model: &source::Model, base: &[ModelCost], name: &str, mode: &source::Mode) -> Vec<ModelCost> {
    let Some((base_default, base_tiers)) = base.split_first() else {
        return Vec::new();
    };
    if let Some(input) = mode.cost.as_ref() {
        let mut result = cost(Some(input));
        result.truncate(1);
        result.extend_from_slice(base_tiers);
        return result;
    }
    let tier = mode.service_tier.as_deref().unwrap_or(name);
    let factor = model
        .cost
        .as_ref()
        .and_then(|cost| cost.multipliers.as_ref())
        .and_then(|multipliers| multipliers.iter().find(|multiplier| service_tier(&multiplier.when) == Some(tier)))
        .map(|multiplier| &multiplier.factor);
    let Some(factor) = factor else {
        return base.to_vec();
    };
    let scale = |price: f64, path: &[&str]| round(price * factor_at(factor, path) * 1e10) / 1e10;
    let mut result = vec![ModelCost {
        tier: None,
        input: money(scale(base_default.input.0, &["text", "input", "price"])),
        output: money(scale(base_default.output.0, &["text", "output", "price"])),
        cache: ModelCostCache {
            read: money(scale(base_default.cache.read.0, &["text", "input", "cache", "read"])),
            write: money(scale(base_default.cache.write.0, &["text", "input", "cache", "write"])),
        },
    }];
    result.extend_from_slice(base_tiers);
    result
}

fn service_tier(when: &Map<String, Value>) -> Option<&str> {
    if when.len() != 1 {
        return None;
    }
    when.get("service_tier")?.as_object()?.get("equals")?.as_str()
}

fn factor_at(factor: &source::Factor, path: &[&str]) -> f64 {
    let mut node = factor;
    for key in path {
        match node {
            source::Factor::Price(_) => break,
            source::Factor::Composite(record) => {
                let Some(next) = record.get(*key) else {
                    return 1.0;
                };
                node = next;
            }
        }
    }
    match node {
        source::Factor::Price(value) => *value,
        source::Factor::Composite(record) => match record.values().next() {
            Some(source::Factor::Price(value)) => *value,
            _ => 1.0,
        },
    }
}

fn round(value: f64) -> f64 {
    let floor = value.floor();
    if value - floor >= 0.5 {
        return floor + 1.0;
    }
    floor
}
