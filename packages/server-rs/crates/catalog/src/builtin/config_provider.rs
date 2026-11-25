use std::sync::{Arc, Mutex, RwLock};

use opencode_config::entry::{Entry, Origin, latest};
use opencode_contract::codec::{Decode, Encode, JsonMap, encode_json};
use opencode_contract::generated::{
    ConfigEntryDocumentInfo, ConfigEntryDocumentInfoModel, ConfigEntryDocumentInfoProvidersValue,
    ConfigEntryDocumentInfoProvidersValueModelsValue, ConfigEntryDocumentInfoProvidersValueModelsValueCost,
    ConfigModelCapabilities, ConfigModelReasoning, ConfigProviderSettings, ConnectionInfo, IntegrationEnvMethod,
    IntegrationEnvMethodType, IntegrationID, IntegrationKeyMethod, IntegrationKeyMethodType, IntegrationMethod,
    ModelCapabilities, ModelCost, ModelCostCache, ModelCostTier, ModelCostTierType, ModelInfo, ModelInfoLimit,
    ModelInfoTime, ModelVariant, ModelVariantSettings, MoneyUSDPerMillionTokens, ProviderID, ProviderInfo,
    ProviderInfoActivation,
};
use opencode_util::time::{Date, parse_rfc3339};
use serde_json::Value;

use crate::aisdk_native;
use crate::builtin::{Builtin, Services};
use crate::integration::Implementation;
use crate::model;
use crate::provider;
use crate::variant;

type ConfiguredProvider = ConfigEntryDocumentInfoProvidersValue;

type ConfiguredModel = ConfigEntryDocumentInfoProvidersValueModelsValue;

#[derive(Clone)]
struct Source {
    inherit: bool,
    base: Option<ModelInfo>,
}

#[derive(Clone)]
struct Definition {
    provider_id: ProviderID,
    models: Vec<(String, Source)>,
}

#[derive(Default)]
struct Sources {
    default_model: Option<ConfigEntryDocumentInfoModel>,
    definitions: Vec<(ConfiguredProvider, Definition)>,
}

struct ModelOverlay<'a> {
    provider_package: Option<&'a str>,
    item: &'a ConfiguredProvider,
    provider_id: &'a ProviderID,
    id: &'a str,
    config: &'a ConfiguredModel,
    inherit: bool,
    base: Option<&'a ModelInfo>,
}

pub async fn register(services: &Services, builtin: &mut Builtin) {
    let entries: Arc<RwLock<Vec<Entry>>> = Arc::new(RwLock::new(Vec::new()));
    let sources: Arc<Mutex<Sources>> = Arc::new(Mutex::new(Sources::default()));
    let handle = {
        let entries = entries.clone();
        let integration = services.integration.clone();
        let provider = services.provider.clone();
        opencode_config::observer::observe(&services.config, move |fresh| {
            *entries.write().expect("lock the configured entries") = fresh.to_vec();
            let integration = integration.clone();
            let provider = provider.clone();
            tokio::spawn(async move {
                integration.reload().await;
                provider.reload().await;
            });
        })
    };
    *entries.write().expect("lock the configured entries") = handle.entries();
    builtin.observers.push(handle);

    let integration_registration = services
        .integration
        .transform({
            let entries = entries.clone();
            move |editor| {
                let entries = entries.read().expect("lock the configured entries").clone();
                for (id, item) in configured_providers(&entries) {
                    let integration_id = IntegrationID(id);
                    if editor.get(&integration_id).is_none() {
                        editor.method_update(Implementation::Method {
                            integration_id: integration_id.clone(),
                            method: IntegrationMethod::Key(IntegrationKeyMethod {
                                r#type: IntegrationKeyMethodType::Key,
                                label: Some("Manually enter API Key".to_owned()),
                                form: None,
                            }),
                        });
                    }
                    editor.update(&integration_id, |integration| {
                        if let Some(name) = item.name.as_ref().filter(|name| !name.is_empty()) {
                            integration.name = name.clone();
                        }
                    });
                    if let Some(env) = &item.env {
                        editor.method_update(Implementation::Method {
                            integration_id,
                            method: IntegrationMethod::Env(IntegrationEnvMethod {
                                r#type: IntegrationEnvMethodType::Env,
                                names: env.clone(),
                            }),
                        });
                    }
                }
                Ok(())
            }
        })
        .await;
    builtin.registrations.push(integration_registration);

    let provider_registration = services
        .provider
        .transform({
            let entries = entries.clone();
            let sources = sources.clone();
            move |editor| {
                let entries = entries.read().expect("lock the configured entries").clone();
                let mut next: Vec<(ConfiguredProvider, Definition)> = Vec::new();
                for (id, item) in configured_providers(&entries) {
                    let provider_id = ProviderID(id);
                    let (current, source, changed) = overlay_provider(editor, &provider_id, &item, None);
                    let mut models: Vec<(String, Source)> = Vec::new();
                    for (model_id, config) in item.models.iter().flat_map(|models| models.0.iter()) {
                        let base = source_base(source.as_ref(), model_id, config.model_id.as_deref());
                        let inherit = changed
                            || !current.as_ref().is_some_and(|definition| definition.models.contains_key(model_id));
                        models.push((model_id.clone(), Source { inherit, base: base.clone() }));
                        if !inherit {
                            continue;
                        }
                        editor.models_update(&provider_id, model_id, |model| {
                            if let Some(base) = &base {
                                *model = base.clone();
                            }
                            if item.package.is_some() {
                                model.package = None;
                            }
                        });
                    }
                    clear_base_url(editor, &provider_id, item.settings.as_ref());
                    next.push((item, Definition { provider_id, models }));
                }
                for (item, definition) in &next {
                    for (model_id, config) in item.models.iter().flat_map(|models| models.0.iter()) {
                        let Some(limit) = &config.limit else {
                            continue;
                        };
                        editor.models_update(&definition.provider_id, model_id, |model| {
                            model.limit = ModelInfoLimit {
                                context: limit.context.unwrap_or(model.limit.context),
                                input: limit.input.or(model.limit.input),
                                output: limit.output.unwrap_or(model.limit.output),
                            };
                        });
                    }
                }
                let mut sources = sources.lock().expect("lock the configured provider sources");
                sources.default_model = latest(&entries, |info| info.model.clone());
                sources.definitions = next;
                Ok(())
            }
        })
        .await;
    builtin.registrations.push(provider_registration);

    let model_registration = services
        .model
        .transform({
            let sources = sources.clone();
            move |editor| {
                let (default_model, definitions) = {
                    let sources = sources.lock().expect("lock the configured provider sources");
                    (sources.default_model.clone(), sources.definitions.clone())
                };
                if let Some(configured) = &default_model {
                    let (provider_id, model_id) = match configured {
                        ConfigEntryDocumentInfoModel::String(value) => model::parse(&value.0),
                        ConfigEntryDocumentInfoModel::Object(value) => {
                            (ProviderID(value.provider_id.clone()), value.model.clone())
                        }
                    };
                    editor.set_default_selection(&provider_id, &model_id);
                }
                for (item, definition) in &definitions {
                    let provider_package =
                        editor.provider(&definition.provider_id).map(|provider| provider.provider.package.clone());
                    for (id, config) in item.models.iter().flat_map(|models| models.0.iter()) {
                        let source =
                            definition.models.iter().find(|(key, _)| key.as_str() == id).map(|(_, source)| source);
                        let (existing, had_variants) = match editor.get(&definition.provider_id, id) {
                            Some(model) => (true, !model.variants.is_empty()),
                            None => (false, false),
                        };
                        let inherit = source.is_some_and(|source| source.inherit) || !existing;
                        let base = source.and_then(|source| source.base.as_ref()).filter(|_| inherit);
                        overlay(
                            editor,
                            ModelOverlay {
                                provider_package: provider_package.as_deref(),
                                item,
                                provider_id: &definition.provider_id,
                                id,
                                config,
                                inherit: base.is_some(),
                                base,
                            },
                        );
                        let source_base = source.and_then(|source| source.base.as_ref());
                        let package = editor.get(&definition.provider_id, id).and_then(|model| model.package.clone());
                        let repackaged =
                            source_base.is_some_and(|base| !base.variants.is_empty() && package != base.package);
                        if config.variants.is_none()
                            && config
                                .capabilities
                                .as_ref()
                                .and_then(|capabilities| capabilities.reasoning.as_ref())
                                .is_none()
                            && ((source_base.is_none() && !had_variants) || repackaged)
                        {
                            editor.update(&definition.provider_id, id, |model| {
                                model.variants = variant::resolve(
                                    &variant_model(model, provider_package.as_deref()),
                                    &[variant::Support::Effort { values: None }],
                                )
                                .iter()
                                .map(contract_variant)
                                .collect();
                            });
                        }
                    }
                }
                Ok(())
            }
        })
        .await;
    builtin.registrations.push(model_registration);
}

pub fn integrate(
    editor: &mut provider::Editor<'_>,
    integration_id: &IntegrationID,
    info: &ConfigEntryDocumentInfo,
    connection: Option<&ConnectionInfo>,
) {
    for (id, item) in info.providers.iter().flat_map(|providers| providers.0.iter()) {
        let provider_id = ProviderID(id.clone());
        let (_, source, _) = overlay_provider(editor, &provider_id, item, Some(integration_id));
        let provider_package = editor.get(&provider_id).map(|definition| definition.provider.package.clone());
        for (id, config) in item.models.iter().flat_map(|models| models.0.iter()) {
            let base = source_base(source.as_ref(), id, config.model_id.as_deref());
            overlay(
                editor,
                ModelOverlay {
                    provider_package: provider_package.as_deref(),
                    item,
                    provider_id: &provider_id,
                    id,
                    config,
                    inherit: true,
                    base: base.as_ref(),
                },
            );
        }
        let Some(configured) = editor.get(&provider_id).cloned() else {
            continue;
        };
        let models: Vec<ModelInfo> = configured.models.values().cloned().collect();
        editor.add(configured.provider, &models, connection.cloned());
    }
}

fn configured_providers(entries: &[Entry]) -> Vec<(String, ConfiguredProvider)> {
    entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Document(document) if !matches!(document.origin, Origin::Integration { .. }) => Some(&document.info),
            _ => None,
        })
        .flat_map(|info| info.providers.iter())
        .flat_map(|providers| providers.0.iter())
        .map(|(id, item)| (id.clone(), item.clone()))
        .collect()
}

fn overlay_provider(
    editor: &mut provider::Editor<'_>,
    provider_id: &ProviderID,
    item: &ConfiguredProvider,
    integration_id: Option<&IntegrationID>,
) -> (Option<provider::Definition>, Option<provider::Definition>, bool) {
    let current = editor.get(provider_id).cloned();
    let source_id = item
        .canonical
        .clone()
        .or_else(|| current.as_ref().and_then(|definition| definition.provider.canonical.clone()))
        .unwrap_or_else(|| provider_id.clone());
    let source = editor.get(&source_id).cloned();
    let changed = item.canonical.is_some()
        && item.canonical != current.as_ref().and_then(|definition| definition.provider.canonical.clone());
    let replacement = match changed {
        true => {
            source.as_ref().filter(|source| source.provider.id != *provider_id).map(|source| source.provider.clone())
        }
        false => None,
    };
    editor.update(provider_id, |provider| {
        if let Some(replacement) = replacement {
            let id = provider.id.clone();
            let integration_id = provider.integration_id.clone();
            *provider = ProviderInfo { id, integration_id, ..replacement };
        }
        if let Some(integration_id) = integration_id {
            provider.integration_id = Some(integration_id.clone());
        }
        provider.activation = ProviderInfoActivation::Enabled;
        if let Some(canonical) = &item.canonical {
            provider.canonical = Some(canonical.clone());
        }
        if let Some(name) = item.name.as_ref().filter(|name| !name.is_empty()) {
            provider.name = name.clone();
        }
        if let Some(package) = &item.package {
            provider.package = package.clone();
        }
        if let Some(compatibility) = &item.compatibility {
            let base = provider.compatibility.clone();
            provider.compatibility = merge_overlay(base.as_ref(), compatibility).or(base);
        }
        if let Some(settings) = &item.settings {
            let base = provider.settings.clone();
            provider.settings = merge_overlay(base.as_ref(), settings).or(base);
        }
        if let Some(headers) = &item.headers {
            provider.headers = merge_headers(provider.headers.as_ref(), headers);
        }
        if let Some(body) = &item.body {
            provider.body = merge_body(provider.body.as_ref(), body);
        }
    });
    (current, source, changed)
}

fn overlay<M: ModelEdits>(models: &mut M, overlay: ModelOverlay<'_>) {
    let ModelOverlay { provider_package, item, provider_id, id, config, inherit, base } = overlay;
    models.update_model(provider_id, id, |model| {
        if inherit {
            if let Some(base) = base {
                *model = base.clone();
            }
            if item.package.is_some() {
                model.package = None;
            }
            if has_base_url(item.settings.as_ref())
                && let Some(settings) = model.settings.as_mut()
            {
                settings.rest.0.retain(|(key, _)| key != "baseURL");
            }
        }
        if let Some(model_type) = &config.r#type {
            model.r#type = Some(model_type.clone());
        }
        if let Some(family) = &config.family {
            model.family = Some(family.clone());
        }
        if let Some(name) = config.name.as_ref().filter(|name| !name.is_empty()) {
            model.name = name.clone();
        }
        if let Some(model_id) = &config.model_id {
            model.model_id = model_id.clone();
        }
        if let Some(release_date) = &config.release_date {
            model.time = ModelInfoTime { released: released(release_date) };
        }
        if let Some(compatibility) = &config.compatibility {
            let base = model.compatibility.clone();
            model.compatibility = merge_overlay(base.as_ref(), compatibility).or(base);
        }
        if let Some(package) = &config.package {
            model.package = Some(package.clone());
        }
        if let Some(settings) = &config.settings {
            let base = model.settings.clone();
            model.settings = merge_overlay(base.as_ref(), settings).or(base);
        }
        if let Some(headers) = &config.headers {
            model.headers = merge_headers(model.headers.as_ref(), headers);
        }
        if let Some(body) = &config.body {
            model.body = merge_body(model.body.as_ref(), body);
        }
        if let Some(capabilities) = &config.capabilities {
            let base = model.capabilities.clone();
            model.capabilities = merge_capabilities(capabilities, &base);
        }
        if let Some(limit) = &config.limit {
            model.limit = ModelInfoLimit {
                context: limit.context.unwrap_or(model.limit.context),
                input: limit.input.or(model.limit.input),
                output: limit.output.unwrap_or(model.limit.output),
            };
        }
    });
    models.update_model(provider_id, id, |model| {
        if let Some(reasoning) = config.capabilities.as_ref().and_then(|capabilities| capabilities.reasoning.as_ref()) {
            model.variants = variant::resolve(&variant_model(model, provider_package), &reasoning_supports(reasoning))
                .iter()
                .map(contract_variant)
                .collect();
        }
        if let Some(variants) = &config.variants {
            for configured in variants {
                let index = match model.variants.iter().position(|existing| existing.id == configured.id) {
                    Some(index) => index,
                    None => {
                        model.variants.push(ModelVariant {
                            id: configured.id.clone(),
                            settings: None,
                            headers: None,
                            body: None,
                        });
                        model.variants.len() - 1
                    }
                };
                let existing = &mut model.variants[index];
                if let Some(settings) = &configured.settings {
                    let base = existing.settings.clone();
                    existing.settings = merge_overlay(base.as_ref(), settings).or(base);
                }
                if let Some(headers) = &configured.headers {
                    existing.headers = merge_headers(existing.headers.as_ref(), headers);
                }
                if let Some(body) = &configured.body {
                    existing.body = merge_body(existing.body.as_ref(), body);
                }
            }
        }
        if let Some(cost) = &config.cost {
            model.cost = model_costs(cost);
        }
        if let Some(disabled) = config.disabled {
            model.enabled = !disabled;
        }
    });
}

fn clear_base_url(
    editor: &mut provider::Editor<'_>,
    provider_id: &ProviderID,
    settings: Option<&ConfigProviderSettings>,
) {
    if !has_base_url(settings) {
        return;
    }
    let ids: Vec<String> = editor
        .get(provider_id)
        .map(|definition| {
            definition
                .models
                .iter()
                .filter(|(_, model)| {
                    model
                        .settings
                        .as_ref()
                        .is_some_and(|settings| settings.rest.0.iter().any(|(key, _)| key == "baseURL"))
                })
                .map(|(id, _)| id.clone())
                .collect()
        })
        .unwrap_or_default();
    for id in ids {
        editor.models_update(provider_id, &id, |model| {
            if let Some(settings) = model.settings.as_mut() {
                settings.rest.0.retain(|(key, _)| key != "baseURL");
            }
        });
    }
}

fn source_base(source: Option<&provider::Definition>, id: &str, model_id: Option<&str>) -> Option<ModelInfo> {
    let source = source?;
    source.models.get(model_id.unwrap_or(id)).or_else(|| source.models.get(id)).cloned()
}

fn has_base_url(settings: Option<&ConfigProviderSettings>) -> bool {
    settings.is_some_and(|settings| settings.rest.0.iter().any(|(key, _)| key == "baseURL"))
}

fn merge_capabilities(config: &ConfigModelCapabilities, base: &ModelCapabilities) -> ModelCapabilities {
    ModelCapabilities {
        tools: config.tools.unwrap_or(base.tools),
        freeform: config.freeform.clone().or_else(|| base.freeform.clone()),
        input: config.input.clone().unwrap_or_else(|| base.input.clone()),
        output: config.output.clone().unwrap_or_else(|| base.output.clone()),
        temperature: config.temperature.or(base.temperature),
    }
}

fn reasoning_supports(reasoning: &ConfigModelReasoning) -> Vec<variant::Support> {
    variant::supports(&match reasoning {
        ConfigModelReasoning::False(_) => variant::Reasoning::default(),
        ConfigModelReasoning::True(reasoning) => variant::Reasoning {
            supported: true,
            toggle: reasoning.toggle.unwrap_or(false),
            effort: reasoning.effort.clone(),
            budget: reasoning.budget.as_ref().map(|budget| variant::Budget {
                min: budget.min.map(|min| min as i64),
                max: budget.max.map(|max| max as i64),
            }),
        },
    })
}

fn variant_model(model: &ModelInfo, provider_package: Option<&str>) -> variant::Model {
    variant::Model {
        id: model.id.clone(),
        model_id: model.model_id.clone(),
        provider_id: model.provider_id.0.clone(),
        package: model.package.clone().or_else(|| provider_package.map(str::to_owned)),
        output: model.limit.output as i64,
    }
}

fn contract_variant(variant: &variant::Variant) -> ModelVariant {
    ModelVariant {
        id: variant.id.clone(),
        settings: variant.settings.as_ref().map(|settings| ModelVariantSettings {
            compaction: None,
            encoding: None,
            rest: JsonMap(settings.iter().map(|(key, value)| (key.clone(), value.clone())).collect()),
        }),
        headers: None,
        body: variant
            .body
            .as_ref()
            .map(|body| JsonMap(body.iter().map(|(key, value)| (key.clone(), value.clone())).collect())),
    }
}

fn model_costs(cost: &ConfigEntryDocumentInfoProvidersValueModelsValueCost) -> Vec<ModelCost> {
    match cost {
        ConfigEntryDocumentInfoProvidersValueModelsValueCost::Object(cost) => vec![model_cost(
            cost.tier.as_ref().map(|tier| tier.size),
            cost.input,
            cost.output,
            cost.cache.as_ref().and_then(|cache| cache.read),
            cost.cache.as_ref().and_then(|cache| cache.write),
        )],
        ConfigEntryDocumentInfoProvidersValueModelsValueCost::Array(costs) => costs
            .iter()
            .map(|cost| {
                model_cost(
                    cost.tier.as_ref().map(|tier| tier.size),
                    cost.input,
                    cost.output,
                    cost.cache.as_ref().and_then(|cache| cache.read),
                    cost.cache.as_ref().and_then(|cache| cache.write),
                )
            })
            .collect(),
    }
}

fn model_cost(
    size: Option<f64>,
    input: MoneyUSDPerMillionTokens,
    output: MoneyUSDPerMillionTokens,
    read: Option<MoneyUSDPerMillionTokens>,
    write: Option<MoneyUSDPerMillionTokens>,
) -> ModelCost {
    ModelCost {
        tier: size.map(|size| ModelCostTier { r#type: ModelCostTierType::Context, size }),
        input,
        output,
        cache: ModelCostCache {
            read: read.unwrap_or(MoneyUSDPerMillionTokens(0.0)),
            write: write.unwrap_or(MoneyUSDPerMillionTokens(0.0)),
        },
    }
}

fn merge_headers(base: Option<&JsonMap<String>>, overlay: &JsonMap<String>) -> Option<JsonMap<String>> {
    let base = base.map(provider::headers_to_native);
    let overlay = provider::headers_to_native(overlay);
    aisdk_native::merge_headers(base.as_ref(), Some(&overlay)).map(provider::headers_from_native)
}

fn merge_body(base: Option<&JsonMap<Value>>, overlay: &JsonMap<Value>) -> Option<JsonMap<Value>> {
    let base = base.map(provider::body_to_native);
    let overlay = provider::body_to_native(overlay);
    aisdk_native::merge_overlay(base.as_ref(), Some(&overlay)).map(provider::body_from_native)
}

fn merge_overlay<T: Decode>(base: Option<&impl Encode>, overlay: &impl Encode) -> Option<T> {
    let base = base.map(json_map);
    let overlay = json_map(overlay);
    provider::decode_object(aisdk_native::merge_overlay(base.as_ref(), Some(&overlay))?)
}

fn json_map(value: &impl Encode) -> aisdk_native::JsonMap {
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&encode_json(value)) else {
        return aisdk_native::JsonMap::new();
    };
    map
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

trait ModelEdits {
    fn update_model(&mut self, provider_id: &ProviderID, model_id: &str, update: impl FnOnce(&mut ModelInfo));
}

impl ModelEdits for model::Editor<'_> {
    fn update_model(&mut self, provider_id: &ProviderID, model_id: &str, update: impl FnOnce(&mut ModelInfo)) {
        self.update(provider_id, model_id, update);
    }
}

impl ModelEdits for provider::Editor<'_> {
    fn update_model(&mut self, provider_id: &ProviderID, model_id: &str, update: impl FnOnce(&mut ModelInfo)) {
        self.models_update(provider_id, model_id, update);
    }
}
