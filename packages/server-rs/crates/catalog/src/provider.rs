use std::collections::BTreeSet;

use opencode_contract::codec::{JsonMap, decode_json, encode_json};
use opencode_contract::generated::{
    ModelCapabilities, ModelCompatibility, ModelCost, ModelInfo, ModelInfoLimit, ModelInfoStatus, ModelInfoTime,
    ModelSettings, ModelType, ModelVariant, ProviderID,
};
use serde_json::{Map, Value};

use crate::aisdk_native::{Context, merge_headers, merge_overlay, options, resolve};
use crate::model::{self, ModelState};

#[derive(Clone, Debug)]
pub(crate) struct ProviderState {
    pub id: String,
    pub name: String,
    pub canonical: Option<String>,
    pub package: Option<String>,
    pub settings: Option<Map<String, Value>>,
    pub headers: Option<Vec<(String, String)>>,
    pub body: Option<Map<String, Value>>,
    pub compatibility: Option<Value>,
    pub models: Vec<ModelState>,
}

impl ProviderState {
    pub(crate) fn new(id: &str) -> Self {
        Self {
            id: id.to_string(),
            name: id.to_string(),
            canonical: None,
            package: None,
            settings: None,
            headers: None,
            body: None,
            compatibility: None,
            models: Vec::new(),
        }
    }

    pub(crate) fn find_model(&self, id: &str) -> Option<&ModelState> {
        self.models.iter().find(|model| model.id == id)
    }

    fn upsert_model(&mut self, model: ModelState) {
        match self.models.iter().position(|existing| existing.id == model.id) {
            Some(position) => self.models[position] = model,
            None => self.models.push(model),
        }
    }

    pub(crate) fn runtime_settings(&self, model: &ModelState) -> Option<Map<String, Value>> {
        let settings = model::model_settings(model.settings.as_ref());
        merge_overlay(self.settings.as_ref(), settings.as_ref())
    }

    pub(crate) fn info(&self, model: &ModelState) -> ModelInfo {
        let compatibility = merge_compatibility(self.compatibility.as_ref(), model.compatibility.as_ref());
        let package = model.package.clone().or_else(|| self.package.clone());
        let provider_settings = model::model_settings(self.settings.as_ref());
        let model_settings = model::model_settings(model.settings.as_ref());
        let settings = merge_overlay(provider_settings.as_ref(), model_settings.as_ref());
        let headers = merge_headers(self.headers.as_deref(), model.headers.as_deref()).map(JsonMap);
        let body =
            merge_overlay(self.body.as_ref(), model.body.as_ref()).map(|body| JsonMap(body.into_iter().collect()));
        ModelInfo {
            id: model.id.clone(),
            model_id: model.model_id.clone(),
            provider_id: ProviderID(self.id.clone()),
            canonical: self.canonical.clone().map(ProviderID),
            r#type: model.r#type.as_deref().map(decode_type),
            family: model.family.clone(),
            name: model.name.clone(),
            compatibility: compatibility.as_ref().map(decode_compatibility),
            package,
            settings: settings.as_ref().map(decode_settings),
            headers,
            body,
            capabilities: decode_capabilities(&model.capabilities),
            variants: model.variants.iter().map(decode_variant).collect(),
            experimental: None,
            time: ModelInfoTime { released: model.released },
            cost: model.cost.iter().map(decode_cost).collect(),
            status: ModelInfoStatus::Active,
            enabled: model.enabled,
            limit: decode_limit(&model.limit),
        }
    }
}

pub(crate) fn apply_provider(
    providers: &mut Vec<ProviderState>,
    provider_id: &str,
    item: &Value,
    builtins: &BTreeSet<&'static str>,
) -> Vec<(String, Value)> {
    let current = providers.iter().find(|provider| provider.id == provider_id).cloned();
    let configured_canonical = item.get("canonical").and_then(Value::as_str);
    let current_canonical = current.as_ref().and_then(|provider| provider.canonical.as_deref());
    let source_id = configured_canonical.or(current_canonical).unwrap_or(provider_id);
    let source = if source_id == provider_id {
        current.clone()
    } else {
        providers.iter().find(|provider| provider.id == source_id).cloned()
    };
    let changed = configured_canonical.is_some() && configured_canonical != current_canonical;
    let mut provider = current.clone().unwrap_or_else(|| ProviderState::new(provider_id));
    if changed
        && let Some(source) = &source
        && source.id != provider_id
    {
        let id = provider.id.clone();
        let models = provider.models.clone();
        provider = source.clone();
        provider.id = id;
        provider.models = models;
    }
    if let Some(canonical) = configured_canonical {
        provider.canonical = Some(canonical.to_string());
    }
    if let Some(name) = item.get("name").and_then(Value::as_str)
        && !name.is_empty()
    {
        provider.name = name.to_string();
    }
    if let Some(package) = item.get("package").and_then(Value::as_str) {
        provider.package = Some(package.to_string());
    }
    if let Some(compatibility) = item.get("compatibility").and_then(Value::as_object) {
        provider.compatibility = Some(model::merge_objects(provider.compatibility.as_ref(), compatibility));
    }
    if let Some(settings) = item.get("settings").and_then(Value::as_object) {
        provider.settings = merge_overlay(provider.settings.as_ref(), Some(settings));
    }
    if let Some(headers) = item.get("headers").and_then(Value::as_object) {
        provider.headers = merge_headers(provider.headers.as_deref(), Some(&pairs(headers)));
    }
    if let Some(body) = item.get("body").and_then(Value::as_object) {
        provider.body = merge_overlay(provider.body.as_ref(), Some(body));
    }
    rewrite_provider(&mut provider, builtins);
    let mut limits = Vec::new();
    if let Some(models) = item.get("models").and_then(Value::as_object) {
        for (id, config) in models {
            let Some(config) = config.as_object() else {
                continue;
            };
            let bound_id = config.get("modelID").and_then(Value::as_str).unwrap_or(id);
            let base = source.as_ref().and_then(|source| source.find_model(bound_id).or_else(|| source.find_model(id)));
            let existing = current.as_ref().and_then(|provider| provider.find_model(id));
            let inherit = changed || existing.is_none();
            let overlay = model::overlay_model(model::Overlay {
                provider_id,
                provider_package: provider.package.as_deref(),
                provider_canonical: provider.canonical.as_deref(),
                id,
                config,
                base,
                existing,
                inherit,
                builtins,
            });
            provider.upsert_model(overlay);
            if let Some(limit) = config.get("limit").and_then(Value::as_object) {
                limits.push((id.clone(), Value::Object(limit.clone())));
            }
        }
    }
    if item.get("settings").and_then(Value::as_object).is_some_and(|settings| settings.contains_key("baseURL")) {
        for model in &mut provider.models {
            if let Some(settings) = &mut model.settings {
                settings.remove("baseURL");
            }
        }
    }
    match providers.iter_mut().find(|existing| existing.id == provider_id) {
        Some(existing) => *existing = provider,
        None => providers.push(provider),
    }
    limits
}

pub(crate) fn apply_limit(providers: &mut [ProviderState], provider_id: &str, model_id: &str, limit: &Value) {
    let Some(model) = providers
        .iter_mut()
        .find(|provider| provider.id == provider_id)
        .and_then(|provider| provider.models.iter_mut().find(|model| model.id == model_id))
    else {
        return;
    };
    let Some(limit) = limit.as_object() else {
        return;
    };
    model.limit = model::merge_objects(Some(&model.limit), limit);
}

fn rewrite_provider(provider: &mut ProviderState, builtins: &BTreeSet<&'static str>) {
    let Some(specifier) = provider.package.clone() else {
        return;
    };
    let settings = provider.settings.clone().unwrap_or_default();
    let context =
        Context { provider_id: &provider.id, model_id: None, shape: None, settings: Some(&settings), builtins };
    let Some(replacement) = resolve(&specifier, &context) else {
        return;
    };
    let translated = options(&settings);
    if provider.package.is_some() || replacement != specifier {
        provider.package = Some(replacement.clone());
    }
    let identity = (replacement == "@opencode/ai/providers/openai-compatible"
        && !builtins.contains(specifier.as_str()))
    .then(|| provider.canonical.clone().unwrap_or_else(|| provider.id.clone()));
    if provider.settings.is_some() {
        let mut merged = translated.settings;
        if let Some(identity) = identity {
            merged.insert("provider".to_string(), Value::String(identity));
        }
        provider.settings = Some(merged);
    }
    provider.headers = merge_headers(translated.headers.as_deref(), provider.headers.as_deref());
    provider.body = merge_overlay(translated.body.as_ref(), provider.body.as_ref());
}

fn merge_compatibility(provider: Option<&Value>, model: Option<&Value>) -> Option<Value> {
    match (provider, model) {
        (None, None) => None,
        (Some(provider), None) => Some(provider.clone()),
        (None, Some(model)) => Some(model.clone()),
        (Some(provider), Some(model)) => match model.as_object() {
            Some(model) => Some(model::merge_objects(Some(provider), model)),
            None => Some(model.clone()),
        },
    }
}

fn pairs(value: &Map<String, Value>) -> Vec<(String, String)> {
    value.iter().filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.to_string()))).collect()
}

fn decode<T: opencode_contract::codec::Decode>(value: &Value) -> T {
    decode_json(&encode_json(value)).expect("catalog value decodes")
}

fn decode_type(value: &str) -> ModelType {
    decode(&Value::String(value.to_string()))
}

fn decode_capabilities(value: &Value) -> ModelCapabilities {
    decode(value)
}

fn decode_compatibility(value: &Value) -> ModelCompatibility {
    decode(value)
}

fn decode_settings(value: &Map<String, Value>) -> ModelSettings {
    decode(&Value::Object(value.clone()))
}

fn decode_variant(value: &Value) -> ModelVariant {
    decode(value)
}

fn decode_cost(value: &Value) -> ModelCost {
    decode(value)
}

fn decode_limit(value: &Value) -> ModelInfoLimit {
    decode(value)
}
