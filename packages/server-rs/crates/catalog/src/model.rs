use std::sync::{Arc, Mutex, Weak};

use indexmap::IndexMap;
use opencode_bus::{Bus, Definition, PublishOptions, Subscription};
use opencode_contract::generated::{
    LocationRef, ModelCompatibility, ModelInfo, ModelInfoStatus, ModelType, ProviderID, ProviderInfo,
};
use opencode_location::state::{self, Options, Registration, State, TransformError};

use crate::aisdk_native;
use crate::integration::ConfigEnv;
use crate::provider::{self, ProviderError, ProviderService, Snapshot};

const UPDATED: &str = "model.updated";

pub const PREFERRED_FREE: [&str; 3] = ["big-pickle", "deepseek-v4.1-flash-free", "muse-spark-1.3-contributor-free"];

#[derive(Debug, Default)]
pub struct ModelRegistry {
    pub(crate) models: IndexMap<String, IndexMap<String, ModelInfo>>,
    pub(crate) default_model: Option<(ProviderID, String)>,
}

pub struct Editor<'a> {
    pub(crate) data: &'a mut ModelRegistry,
    pub(crate) input: Option<&'a Snapshot>,
}

impl Editor<'_> {
    pub fn list(&mut self, provider_id: Option<&ProviderID>) -> Vec<&mut ModelInfo> {
        match provider_id {
            Some(provider_id) => match self.data.models.get_mut(&provider_id.0) {
                Some(models) => models.values_mut().collect(),
                None => Vec::new(),
            },
            None => self.data.models.values_mut().flat_map(|models| models.values_mut()).collect(),
        }
    }

    pub fn get(&mut self, provider_id: &ProviderID, model_id: &str) -> Option<&mut ModelInfo> {
        self.data.models.get_mut(&provider_id.0).and_then(|models| models.get_mut(model_id))
    }

    pub fn update(&mut self, provider_id: &ProviderID, model_id: &str, update: impl FnOnce(&mut ModelInfo)) {
        let Some(models) = self.data.models.get_mut(&provider_id.0) else {
            return;
        };
        let mut model = models.get(model_id).cloned().unwrap_or_else(|| provider::default_model(provider_id, model_id));
        update(&mut model);
        model.id = model_id.to_owned();
        model.provider_id = provider_id.clone();
        let provider =
            self.input.and_then(|snapshot| snapshot.records.get(&provider_id.0)).map(|definition| &definition.provider);
        provider::rewrite_model(&mut model, provider_id, provider);
        models.insert(model_id.to_owned(), model);
    }

    pub fn remove(&mut self, provider_id: &ProviderID, model_id: &str) {
        let Some(models) = self.data.models.get_mut(&provider_id.0) else {
            return;
        };
        models.shift_remove(model_id);
    }

    pub fn default_selection(&self) -> Option<(ProviderID, String)> {
        self.data.default_model.clone()
    }

    pub fn set_default_selection(&mut self, provider_id: &ProviderID, model_id: &str) {
        self.data.default_model = Some((provider_id.clone(), model_id.to_owned()));
    }

    pub fn provider_list(&self) -> Vec<provider::Definition> {
        self.input.map(|snapshot| snapshot.records.entries.values().cloned().collect()).unwrap_or_default()
    }

    pub fn provider(&self, provider_id: &ProviderID) -> Option<&provider::Definition> {
        self.input.and_then(|snapshot| snapshot.records.get(&provider_id.0))
    }
}

#[derive(Debug)]
pub struct ModelSnapshot {
    pub default_model: Option<(ProviderID, String)>,
    pub all: Vec<ModelInfo>,
    pub available: Vec<ModelInfo>,
    pub other: IndexMap<String, Vec<ModelInfo>>,
    pub by_provider: IndexMap<String, IndexMap<String, ModelInfo>>,
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error(transparent)]
    State(#[from] opencode_location::state::StateError),
    #[error(transparent)]
    Provider(#[from] ProviderError),
}

pub struct ModelService {
    providers: Arc<ProviderService>,
    state: State<ModelRegistry>,
    bus: Arc<Bus>,
    location: LocationRef,
    input: Arc<Mutex<Option<Arc<Snapshot>>>>,
    cached: Mutex<Option<Cached>>,
    published: Mutex<Option<Arc<ModelSnapshot>>>,
    disable_free: bool,
    subscription: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

struct Cached {
    data: Arc<ModelRegistry>,
    value: Arc<ModelSnapshot>,
}

impl ModelService {
    pub fn new(
        providers: Arc<ProviderService>,
        bus: Arc<Bus>,
        location: LocationRef,
        environment: ConfigEnv,
    ) -> Arc<Self> {
        let disable_free = opencode_config::options::truthy(environment("OPENCODE_DISABLE_FREE_MODELS").as_deref());
        let input: Arc<Mutex<Option<Arc<Snapshot>>>> = Arc::new(Mutex::new(None));
        let initial_input = input.clone();
        Arc::new_cyclic(|weak: &Weak<ModelService>| {
            let notify_weak = weak.clone();
            let notify: state::NotifyFn<ModelRegistry> = Box::new(move |_| {
                let weak = notify_weak.clone();
                Box::pin(async move {
                    let Some(service) = weak.upgrade() else {
                        return;
                    };
                    service.notify().await;
                })
            });
            let initial = initial_input.clone();
            let options = Options {
                name: Some("model".to_owned()),
                initial: Box::new(move || {
                    let current = initial.lock().expect("lock the model input").clone();
                    ModelRegistry {
                        models: current
                            .as_ref()
                            .map(|snapshot| {
                                snapshot
                                    .available
                                    .iter()
                                    .map(|definition| (definition.provider.id.0.clone(), definition.models.clone()))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        default_model: None,
                    }
                }),
                notify: Some(notify),
            };
            let subscription = bus.subscribe(&[provider::UPDATED], None);
            let subscription = tokio::spawn(subscription_task(subscription, weak.clone()));
            Self {
                providers,
                state: State::new(options),
                bus,
                location,
                input,
                cached: Mutex::new(None),
                published: Mutex::new(None),
                disable_free,
                subscription: Mutex::new(Some(subscription)),
            }
        })
    }

    pub async fn transform<F>(&self, f: F) -> Registration
    where
        F: for<'a> Fn(&mut Editor<'a>) -> Result<(), TransformError> + Send + Sync + 'static,
    {
        if let Err(error) = self.prepare().await {
            log::warn!("model transform was not prepared: {error}");
        }
        let input = self.input.clone();
        self.state
            .transform(move |data| {
                let current = input.lock().expect("lock the model input").clone();
                f(&mut Editor { data, input: current.as_deref() })
            })
            .await
    }

    pub async fn reload(&self) -> Result<(), ModelError> {
        self.prepare().await?;
        self.state.reload().await;
        Ok(())
    }

    pub async fn snapshot(&self) -> Result<Arc<ModelSnapshot>, ModelError> {
        self.read().await
    }

    pub async fn get(&self, provider_id: &ProviderID, model_id: &str) -> Result<Option<ModelInfo>, ModelError> {
        let value = self.read().await?;
        Ok(value.by_provider.get(&provider_id.0).and_then(|models| models.get(model_id)).cloned())
    }

    pub async fn all(&self) -> Result<Vec<ModelInfo>, ModelError> {
        Ok(self.read().await?.all.clone())
    }

    pub async fn available(&self, types: Option<&[ModelType]>) -> Result<Vec<ModelInfo>, ModelError> {
        let value = self.read().await?;
        let Some(types) = types.filter(|types| !types.is_empty()) else {
            return Ok(value.available.clone());
        };
        let mut seen: Vec<&str> = Vec::new();
        let mut models = Vec::new();
        for model_type in types {
            let key = type_key(model_type);
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            if key == "chat" {
                models.extend(value.available.iter().cloned());
            } else if let Some(typed) = value.other.get(key) {
                models.extend(typed.iter().cloned());
            }
        }
        Ok(models)
    }

    pub async fn default_model(&self) -> Result<Option<ModelInfo>, ModelError> {
        let value = self.read().await?;
        if let Some((provider_id, model_id)) = &value.default_model
            && let Some(model) = value.by_provider.get(&provider_id.0).and_then(|models| models.get(model_id))
            && model.enabled
        {
            return Ok(Some(model.clone()));
        }
        Ok(value.available.iter().find(|model| supports_text(model)).cloned())
    }

    pub async fn small(&self, provider_id: &ProviderID) -> Result<Option<ModelInfo>, ModelError> {
        let value = self.read().await?;
        let candidates: Vec<&ModelInfo> = value
            .available
            .iter()
            .filter(|model| {
                model.provider_id == *provider_id
                    && model.status == ModelInfoStatus::Active
                    && model.capabilities.input.iter().any(|item| item.starts_with("text"))
                    && model.capabilities.output.iter().any(|item| item.starts_with("text"))
            })
            .collect();
        for family in ["gpt-luna", "gemini-flash-lite", "gemini-flash", "claude-haiku"] {
            if let Some(model) = candidates.iter().find(|model| model.family.as_deref() == Some(family)) {
                return Ok(Some((*model).clone()));
            }
        }
        Ok(None)
    }

    async fn prepare(&self) -> Result<(), ModelError> {
        let current = self.providers.snapshot().await?;
        self.observe(&current);
        Ok(())
    }

    fn observe(&self, current: &Arc<Snapshot>) {
        {
            let mut input = self.input.lock().expect("lock the model input");
            if input.as_ref().is_some_and(|value| Arc::ptr_eq(value, current)) {
                return;
            }
            *input = Some(current.clone());
        }
        self.state.invalidate();
    }

    async fn read(&self) -> Result<Arc<ModelSnapshot>, ModelError> {
        loop {
            let current = self.providers.snapshot().await?;
            self.observe(&current);
            let data = self.state.get().await?;
            if let Some(value) = self.cached_value(&data) {
                return Ok(value);
            }
            if !Arc::ptr_eq(&current, &self.providers.snapshot().await?) {
                continue;
            }
            let value = Arc::new(build(&data, &current, self.disable_free));
            *self.cached.lock().expect("lock the model snapshot cache") = Some(Cached { data, value: value.clone() });
            return Ok(value);
        }
    }

    fn cached_value(&self, data: &Arc<ModelRegistry>) -> Option<Arc<ModelSnapshot>> {
        let cached = self.cached.lock().expect("lock the model snapshot cache");
        cached.as_ref().filter(|entry| Arc::ptr_eq(&entry.data, data)).map(|entry| entry.value.clone())
    }

    async fn notify(&self) {
        let value = match self.read().await {
            Ok(value) => value,
            Err(error) => {
                log::warn!("model update event was not prepared: {error}");
                return;
            }
        };
        {
            let mut published = self.published.lock().expect("lock the published model snapshot");
            if published.as_ref().is_some_and(|current| Arc::ptr_eq(current, &value)) {
                return;
            }
            *published = Some(value);
        }
        let definition = Definition::ephemeral(UPDATED);
        let options = PublishOptions { location: Some(self.location.clone()), ..Default::default() };
        if let Err(error) = self.bus.publish(&definition, serde_json::json!({}), options) {
            log::warn!("model update event was not published: {error}");
        }
    }
}

impl Drop for ModelService {
    fn drop(&mut self) {
        if let Ok(mut subscription) = self.subscription.lock()
            && let Some(task) = subscription.take()
        {
            task.abort();
        }
    }
}

async fn subscription_task(mut subscription: Subscription, weak: Weak<ModelService>) {
    while subscription.recv().await.is_some() {
        let Some(service) = weak.upgrade() else {
            break;
        };
        service.notify().await;
    }
}

fn build(data: &ModelRegistry, input: &Snapshot, disable_free: bool) -> ModelSnapshot {
    let mut by_provider: IndexMap<String, IndexMap<String, ModelInfo>> = IndexMap::new();
    for (provider_id, models) in &data.models {
        let provider = input.records.get(provider_id).map(|definition| &definition.provider);
        let merged: IndexMap<String, ModelInfo> =
            models.iter().map(|(id, model)| (id.clone(), merge_model(model, provider))).collect();
        by_provider.insert(provider_id.clone(), merged);
    }
    if disable_free {
        for provider_id in ["opencode", "opencode-go"] {
            if let Some(models) = by_provider.get_mut(provider_id) {
                models.retain(|_, model| model.cost.iter().any(|cost| cost.input.0 > 0.0));
            }
        }
    }
    let mut other: IndexMap<String, Vec<ModelInfo>> = IndexMap::new();
    for models in by_provider.values_mut() {
        let keys: Vec<String> =
            models.iter().filter(|(_, model)| type_of(model) != "chat").map(|(id, _)| id.clone()).collect();
        for key in keys {
            let model = models.shift_remove(&key).expect("a non-chat model");
            if model.enabled {
                let name = type_of(&model).to_owned();
                other.entry(name).or_default().push(model);
            }
        }
    }
    let mut all: Vec<ModelInfo> = by_provider.values().flat_map(|models| models.values().cloned()).collect();
    all.sort_by(|left, right| right.time.released.total_cmp(&left.time.released));
    for models in other.values_mut() {
        models.sort_by(|left, right| right.time.released.total_cmp(&left.time.released));
    }
    let available = prefer_free(all.iter().filter(|model| model.enabled).cloned().collect());
    ModelSnapshot { default_model: data.default_model.clone(), all, available, other, by_provider }
}

fn merge_model(model: &ModelInfo, provider: Option<&ProviderInfo>) -> ModelInfo {
    let provider_headers = provider.and_then(|provider| provider.headers.as_ref()).map(provider::headers_to_native);
    let model_headers = model.headers.as_ref().map(provider::headers_to_native);
    let provider_body = provider.and_then(|provider| provider.body.as_ref()).map(provider::body_to_native);
    let model_body = model.body.as_ref().map(provider::body_to_native);
    let canonical = provider.and_then(|provider| provider.canonical.clone()).or_else(|| model.canonical.clone());
    let compatibility = resolve_compatibility(provider, model);
    let package = model.package.clone().or_else(|| provider.map(|provider| provider.package.clone()));
    let settings = provider::merge_model_settings(
        provider.and_then(|provider| provider.settings.as_ref()),
        model.settings.as_ref(),
    );
    let headers = aisdk_native::merge_headers(provider_headers.as_ref(), model_headers.as_ref())
        .map(provider::headers_from_native);
    let body = aisdk_native::merge_overlay(provider_body.as_ref(), model_body.as_ref()).map(provider::body_from_native);
    ModelInfo { canonical, compatibility, package, settings, headers, body, ..model.clone() }
}

fn resolve_compatibility(provider: Option<&ProviderInfo>, model: &ModelInfo) -> Option<ModelCompatibility> {
    let Some(provider) = provider.and_then(|provider| provider.compatibility.as_ref()) else {
        return model.compatibility.clone();
    };
    let overlay = model.compatibility.as_ref();
    Some(ModelCompatibility {
        reasoning_field: overlay
            .and_then(|value| value.reasoning_field.clone())
            .or_else(|| provider.reasoning_field.clone()),
        require_reasoning: overlay.and_then(|value| value.require_reasoning).or(provider.require_reasoning),
        max_tokens_field: overlay.and_then(|value| value.max_tokens_field).or(provider.max_tokens_field),
        require_finish_reason: overlay.and_then(|value| value.require_finish_reason).or(provider.require_finish_reason),
        require_assistant_after_tool: overlay
            .and_then(|value| value.require_assistant_after_tool)
            .or(provider.require_assistant_after_tool),
        supports_prompt_cache_key: overlay
            .and_then(|value| value.supports_prompt_cache_key)
            .or(provider.supports_prompt_cache_key),
        stream_usage: overlay.and_then(|value| value.stream_usage).or(provider.stream_usage),
        cache_format: overlay.and_then(|value| value.cache_format).or(provider.cache_format),
        supports_thinking_block_binding: overlay.and_then(|value| value.supports_thinking_block_binding),
        supports_effort_updates: overlay.and_then(|value| value.supports_effort_updates),
        media_input: overlay.and_then(|value| value.media_input).or(provider.media_input),
    })
}

fn prefer_free(models: Vec<ModelInfo>) -> Vec<ModelInfo> {
    let free_only = models
        .iter()
        .all(|model| model.provider_id.0 == "opencode" && !model.cost.iter().any(|cost| cost.input.0 > 0.0));
    if !free_only {
        return models;
    }
    let mut picks: Vec<ModelInfo> = Vec::new();
    for id in PREFERRED_FREE {
        if let Some(model) = models.iter().find(|model| model.id == id) {
            picks.push(model.clone());
        }
    }
    let picked: Vec<String> = picks.iter().map(|model| model.id.clone()).collect();
    picks.extend(models.into_iter().filter(|model| !picked.contains(&model.id)));
    picks
}

pub fn supports_text(model: &ModelInfo) -> bool {
    (model.capabilities.input.is_empty() || model.capabilities.input.iter().any(|item| item.starts_with("text")))
        && (model.capabilities.output.is_empty()
            || model.capabilities.output.iter().any(|item| item.starts_with("text")))
}

pub fn parse(input: &str) -> Option<(ProviderID, String)> {
    let (provider_id, rest) = input.split_once('/')?;
    let (model_id, variant) = match rest.split_once('#') {
        Some((model_id, variant)) => (model_id, Some(variant)),
        None => (rest, None),
    };
    if provider_id.is_empty() || provider_id.contains('#') || model_id.is_empty() || variant.is_some_and(str::is_empty)
    {
        return None;
    }
    Some((ProviderID(provider_id.to_owned()), model_id.to_owned()))
}

fn type_key(model_type: &ModelType) -> &str {
    match model_type {
        ModelType::String(_) => "chat",
        ModelType::String2(_) => "decision",
        ModelType::String3(_) => "image",
        ModelType::String4(_) => "video",
        ModelType::String5(value) => value,
    }
}

fn type_of(model: &ModelInfo) -> &str {
    model.r#type.as_ref().map(type_key).unwrap_or("chat")
}
