use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, LazyLock, Mutex, Weak};

use indexmap::IndexMap;
use opencode_ai::ProviderPackages;
use opencode_bus::{Bus, PublishOptions, Subscription};
use opencode_contract::codec::{self, Decode, Encode, encode_json};
use opencode_contract::generated::{
    ConnectionInfo, IntegrationID, IntegrationInfo, LocationRef, ModelCapabilities, ModelInfo, ModelInfoLimit,
    ModelInfoStatus, ModelInfoTime, ModelSettings, ModelVariant, ProviderEncoding, ProviderID, ProviderInfo,
    ProviderInfoActivation, ProviderSettings, ProviderSettingsChunkTimeout, ProviderSettingsHeaderTimeout,
    ProviderSettingsTimeout, ProviderTls,
};
use opencode_location::state::{self, Options, Registration, State, TransformError};
use serde_json::Value;

use crate::aisdk_native::{self, Context, Target};
use crate::integration::{IntegrationError, IntegrationService};

pub(crate) const UPDATED: &str = "provider.updated";

const SUBSCRIBED: [&str; 2] = [crate::integration::UPDATED, "credential.switched"];

pub const DEFAULT_TIMEOUT_MS: f64 = 300_000.0;

pub const DEFAULT_CHUNK_TIMEOUT_MS: f64 = 900_000.0;

static BUILTIN_PACKAGES: LazyLock<BTreeSet<&'static str>> = LazyLock::new(|| ProviderPackages::builtin().specifiers());

#[derive(Debug, Clone, PartialEq)]
pub struct Definition {
    pub provider: ProviderInfo,
    pub models: IndexMap<String, ModelInfo>,
    pub source_connection: Option<ConnectionInfo>,
}

#[derive(Default, Debug)]
pub struct ProviderRegistry {
    pub(crate) entries: IndexMap<String, Definition>,
}

impl ProviderRegistry {
    pub fn get(&self, id: &str) -> Option<&Definition> {
        self.entries.get(id)
    }
}

pub struct Editor<'a> {
    pub(crate) registry: &'a mut ProviderRegistry,
}

impl Editor<'_> {
    pub fn list(&self) -> Vec<Definition> {
        self.registry.entries.values().cloned().collect()
    }

    pub fn get(&self, id: &ProviderID) -> Option<&Definition> {
        self.registry.entries.get(&id.0)
    }

    pub fn add(&mut self, info: ProviderInfo, models: &[ModelInfo], source_connection: Option<ConnectionInfo>) {
        let definition = Definition { provider: info.clone(), models: index(&info.id, models), source_connection };
        self.registry.entries.insert(info.id.0.clone(), definition);
    }

    pub fn update(&mut self, id: &ProviderID, update: impl FnOnce(&mut ProviderInfo)) {
        let entry = self.ensure(id);
        update(&mut entry.provider);
        entry.provider.id = id.clone();
        rewrite_provider(&mut entry.provider);
    }

    pub fn remove(&mut self, id: &ProviderID) {
        self.registry.entries.shift_remove(&id.0);
    }

    pub fn models_set(&mut self, id: &ProviderID, models: &[ModelInfo]) {
        self.ensure(id).models = index(id, models);
    }

    pub fn models_update(&mut self, provider_id: &ProviderID, model_id: &str, update: impl FnOnce(&mut ModelInfo)) {
        let entry = self.ensure(provider_id);
        let provider = entry.provider.clone();
        let mut model = entry.models.get(model_id).cloned().unwrap_or_else(|| default_model(provider_id, model_id));
        update(&mut model);
        model.id = model_id.to_owned();
        model.provider_id = provider_id.clone();
        rewrite_model(&mut model, provider_id, Some(&provider));
        entry.models.insert(model_id.to_owned(), model);
    }

    pub fn models_remove(&mut self, provider_id: &ProviderID, model_id: &str) {
        let Some(entry) = self.registry.entries.get_mut(&provider_id.0) else {
            return;
        };
        entry.models.shift_remove(model_id);
    }

    fn ensure(&mut self, id: &ProviderID) -> &mut Definition {
        self.registry.entries.entry(id.0.clone()).or_insert_with(|| Definition {
            provider: ProviderInfo {
                id: id.clone(),
                canonical: None,
                integration_id: None,
                name: id.0.clone(),
                activation: ProviderInfoActivation::Auto,
                package: String::new(),
                compatibility: None,
                settings: None,
                headers: None,
                body: None,
            },
            models: IndexMap::new(),
            source_connection: None,
        })
    }
}

pub struct Snapshot {
    pub records: Arc<ProviderRegistry>,
    pub available: Vec<Definition>,
    pub providers: Vec<ProviderInfo>,
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error(transparent)]
    State(#[from] opencode_location::state::StateError),
    #[error(transparent)]
    Integration(#[from] IntegrationError),
}

pub struct ProviderService {
    state: State<ProviderRegistry>,
    integrations: Arc<IntegrationService>,
    bus: Arc<Bus>,
    location: LocationRef,
    cached: Mutex<Option<Cached>>,
    published: Mutex<Option<Arc<Snapshot>>>,
    subscription: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

struct Cached {
    records: Arc<ProviderRegistry>,
    value: Arc<Snapshot>,
}

impl ProviderService {
    pub fn new(integrations: Arc<IntegrationService>, bus: Arc<Bus>, location: LocationRef) -> Arc<Self> {
        Arc::new_cyclic(|weak: &Weak<ProviderService>| {
            let notify_weak = weak.clone();
            let notify: state::NotifyFn<ProviderRegistry> = Box::new(move |_| {
                let weak = notify_weak.clone();
                Box::pin(async move {
                    let Some(service) = weak.upgrade() else {
                        return;
                    };
                    service.notify().await;
                })
            });
            let options = Options {
                name: Some("provider".to_owned()),
                initial: Box::new(ProviderRegistry::default),
                notify: Some(notify),
            };
            let subscription = bus.subscribe(&SUBSCRIBED, None);
            let subscription = tokio::spawn(subscription_task(subscription, weak.clone()));
            Self {
                state: State::new(options),
                integrations,
                bus,
                location,
                cached: Mutex::new(None),
                published: Mutex::new(None),
                subscription: Mutex::new(Some(subscription)),
            }
        })
    }

    pub async fn transform<F>(&self, f: F) -> Registration
    where
        F: for<'a> Fn(&mut Editor<'a>) -> Result<(), TransformError> + Send + Sync + 'static,
    {
        self.state.transform(move |registry| f(&mut Editor { registry })).await
    }

    pub async fn block(
        &self,
        block: Arc<dyn opencode_location::state::ExternalBlock<ProviderRegistry>>,
    ) -> Registration {
        self.state.block(block).await
    }

    pub async fn reload(&self) {
        self.state.reload().await;
    }

    pub async fn get(&self, id: &ProviderID) -> Result<Option<ProviderInfo>, ProviderError> {
        let registry = self.state.get().await?;
        Ok(registry.get(&id.0).map(|definition| definition.provider.clone()))
    }

    pub async fn all(&self) -> Result<Vec<ProviderInfo>, ProviderError> {
        let registry = self.state.get().await?;
        Ok(registry.entries.values().map(|definition| definition.provider.clone()).collect())
    }

    pub async fn available(&self) -> Result<Vec<ProviderInfo>, ProviderError> {
        Ok(self.snapshot().await?.providers.clone())
    }

    pub async fn snapshot(&self) -> Result<Arc<Snapshot>, ProviderError> {
        loop {
            let revision = self.integrations.revision();
            let records = self.state.get().await?;
            let connections = self.integrations.list().await?;
            if revision != self.integrations.revision() || !Arc::ptr_eq(&records, &self.state.get().await?) {
                continue;
            }
            let by_id: HashMap<String, IntegrationInfo> =
                connections.into_iter().map(|integration| (integration.id.0.clone(), integration)).collect();
            let available: Vec<&Definition> =
                records.entries.values().filter(|definition| is_available(definition, &by_id)).collect();
            let mut cached = self.cached.lock().expect("lock the provider snapshot cache");
            if let Some(entry) = cached.as_ref()
                && Arc::ptr_eq(&entry.records, &records)
                && entry.value.available.len() == available.len()
                && entry
                    .value
                    .available
                    .iter()
                    .zip(&available)
                    .all(|(left, right)| left.provider.id == right.provider.id)
            {
                return Ok(entry.value.clone());
            }
            let providers = available.iter().map(|definition| definition.provider.clone()).collect();
            let available = available.into_iter().cloned().collect();
            let value = Arc::new(Snapshot { records: records.clone(), available, providers });
            *cached = Some(Cached { records, value: value.clone() });
            return Ok(value);
        }
    }

    async fn notify(&self) {
        let value = match self.snapshot().await {
            Ok(value) => value,
            Err(error) => {
                log::warn!("provider update event was not prepared: {error}");
                return;
            }
        };
        {
            let mut published = self.published.lock().expect("lock the published provider snapshot");
            if published.as_ref().is_some_and(|current| Arc::ptr_eq(current, &value)) {
                return;
            }
            *published = Some(value);
        }
        let definition = opencode_bus::Definition::ephemeral(UPDATED);
        let options = PublishOptions { location: Some(self.location.clone()), ..Default::default() };
        if let Err(error) = self.bus.publish(&definition, serde_json::json!({}), options) {
            log::warn!("provider update event was not published: {error}");
        }
    }
}

impl Drop for ProviderService {
    fn drop(&mut self) {
        if let Ok(mut subscription) = self.subscription.lock()
            && let Some(task) = subscription.take()
        {
            task.abort();
        }
    }
}

async fn subscription_task(mut subscription: Subscription, weak: Weak<ProviderService>) {
    while subscription.recv().await.is_some() {
        let Some(service) = weak.upgrade() else {
            break;
        };
        service.notify().await;
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HttpTimeout {
    Millis(f64),
    Disabled,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timeouts {
    pub timeout: Option<HttpTimeout>,
    pub header_timeout: HttpTimeout,
    pub chunk_timeout: HttpTimeout,
}

pub fn native_settings(settings: &ProviderSettings) -> aisdk_native::JsonMap {
    settings.rest.0.iter().cloned().collect()
}

pub fn model_settings(settings: Option<&ProviderSettings>) -> Option<ModelSettings> {
    settings.map(|settings| ModelSettings {
        compaction: settings.compaction.clone(),
        cache: settings.cache.clone(),
        encoding: settings.encoding,
        rest: settings.rest.clone(),
    })
}

pub(crate) fn merge_model_settings(
    provider: Option<&ProviderSettings>,
    model: Option<&ModelSettings>,
) -> Option<ModelSettings> {
    let base = model_settings(provider).as_ref().map(json_object);
    let overlay = model.map(json_object);
    let merged = aisdk_native::merge_overlay(base.as_ref(), overlay.as_ref())?;
    Some(decode_object(merged))
}

pub fn timeout(value: Option<&ProviderSettingsTimeout>) -> Option<HttpTimeout> {
    match value? {
        ProviderSettingsTimeout::Number(number) => millis(number.0),
        ProviderSettingsTimeout::Bool(_) => Some(HttpTimeout::Disabled),
    }
}

pub fn timeouts(settings: &ProviderSettings) -> Timeouts {
    let header_timeout = match settings.header_timeout.as_ref() {
        Some(ProviderSettingsHeaderTimeout::Number(number)) => millis(number.0),
        Some(ProviderSettingsHeaderTimeout::Bool(_)) => Some(HttpTimeout::Disabled),
        None => None,
    }
    .unwrap_or(HttpTimeout::Millis(DEFAULT_TIMEOUT_MS));
    let chunk_timeout = match settings.chunk_timeout.as_ref() {
        Some(ProviderSettingsChunkTimeout::Number(number)) => millis(number.0),
        Some(ProviderSettingsChunkTimeout::Bool(_)) => Some(HttpTimeout::Disabled),
        None => None,
    }
    .unwrap_or(HttpTimeout::Millis(DEFAULT_CHUNK_TIMEOUT_MS));
    Timeouts { timeout: timeout(settings.timeout.as_ref()), header_timeout, chunk_timeout }
}

pub fn encoding(settings: &ProviderSettings) -> Option<ProviderEncoding> {
    settings.encoding
}

pub fn tls(settings: &ProviderSettings) -> Option<ProviderTls> {
    settings.tls.clone().filter(|tls| !tls.cert.is_empty() && !tls.key.is_empty())
}

pub(crate) fn builtin_packages() -> &'static BTreeSet<&'static str> {
    &BUILTIN_PACKAGES
}

pub(crate) fn default_model(provider_id: &ProviderID, model_id: &str) -> ModelInfo {
    ModelInfo {
        id: model_id.to_owned(),
        model_id: model_id.to_owned(),
        provider_id: provider_id.clone(),
        canonical: None,
        r#type: None,
        family: None,
        name: model_id.to_owned(),
        compatibility: None,
        package: None,
        settings: None,
        headers: None,
        body: None,
        capabilities: ModelCapabilities {
            tools: true,
            freeform: None,
            input: vec!["text".to_owned(), "image".to_owned()],
            output: vec!["text".to_owned()],
            temperature: None,
        },
        variants: Vec::new(),
        time: ModelInfoTime { released: 0.0 },
        cost: Vec::new(),
        status: ModelInfoStatus::Active,
        enabled: true,
        limit: ModelInfoLimit { context: 200_000.0, input: None, output: 32_000.0 },
    }
}

fn millis(value: f64) -> Option<HttpTimeout> {
    (value > 0.0).then_some(HttpTimeout::Millis(value))
}

fn index(provider_id: &ProviderID, models: &[ModelInfo]) -> IndexMap<String, ModelInfo> {
    models
        .iter()
        .map(|model| {
            let owned = if model.provider_id == *provider_id {
                model.clone()
            } else {
                ModelInfo { provider_id: provider_id.clone(), ..model.clone() }
            };
            (owned.id.clone(), owned)
        })
        .collect()
}

fn rewrite_provider(provider: &mut ProviderInfo) {
    let mut target = Target {
        settings: provider.settings.as_ref().map(json_object),
        headers: provider.headers.as_ref().map(headers_to_native),
        body: provider.body.as_ref().map(body_to_native),
        package: Some(provider.package.clone()),
        variants: None,
    };
    let context = Context {
        provider_id: provider.id.0.clone(),
        canonical: provider.canonical.as_ref().map(|canonical| canonical.0.clone()),
        model_id: None,
        shape: None,
        settings: None,
        specifier: Some(provider.package.clone()),
    };
    aisdk_native::rewrite(&mut target, &context, builtin_packages());
    provider.package = target.package.unwrap_or_default();
    provider.settings = target.settings.map(decode_object);
    provider.headers = target.headers.map(headers_from_native);
    provider.body = target.body.map(body_from_native);
}

pub(crate) fn rewrite_model(model: &mut ModelInfo, provider_id: &ProviderID, provider: Option<&ProviderInfo>) {
    let mut target = Target {
        settings: model.settings.as_ref().map(json_object),
        headers: model.headers.as_ref().map(headers_to_native),
        body: model.body.as_ref().map(body_to_native),
        package: model.package.clone(),
        variants: Some(model.variants.iter().map(native_variant).collect()),
    };
    let context = Context {
        provider_id: provider_id.0.clone(),
        canonical: model
            .canonical
            .clone()
            .or_else(|| provider.and_then(|provider| provider.canonical.clone()))
            .map(|canonical| canonical.0),
        model_id: Some(model.model_id.clone()),
        shape: None,
        settings: None,
        specifier: model.package.clone().or_else(|| provider.map(|provider| provider.package.clone())),
    };
    aisdk_native::rewrite(&mut target, &context, builtin_packages());
    model.package = target.package;
    model.settings = target.settings.map(decode_object);
    model.headers = target.headers.map(headers_from_native);
    model.body = target.body.map(body_from_native);
    if let Some(variants) = target.variants {
        model.variants = variants.into_iter().map(contract_variant).collect();
    }
}

fn native_variant(variant: &ModelVariant) -> aisdk_native::Variant {
    aisdk_native::Variant {
        id: variant.id.clone(),
        settings: variant.settings.as_ref().map(json_object),
        headers: variant.headers.as_ref().map(headers_to_native),
        body: variant.body.as_ref().map(body_to_native),
    }
}

fn contract_variant(variant: aisdk_native::Variant) -> ModelVariant {
    ModelVariant {
        id: variant.id,
        settings: variant.settings.map(decode_object),
        headers: variant.headers.map(headers_from_native),
        body: variant.body.map(body_from_native),
    }
}

fn is_available(definition: &Definition, by_id: &HashMap<String, IntegrationInfo>) -> bool {
    if definition.provider.activation == ProviderInfoActivation::Disabled {
        return false;
    }
    let integration_id =
        definition.provider.integration_id.clone().unwrap_or_else(|| IntegrationID(definition.provider.id.0.clone()));
    let integration = by_id.get(&integration_id.0);
    if let Some(source) = &definition.source_connection
        && Some(crate::integration::key(source))
            != integration.and_then(|integration| integration.connections.first()).map(crate::integration::key)
    {
        return false;
    }
    if definition.provider.activation == ProviderInfoActivation::Enabled {
        return true;
    }
    if integration.is_some_and(|integration| !integration.connections.is_empty()) {
        return true;
    }
    definition.provider.integration_id.is_none() && integration.is_none()
}

pub(crate) fn json_object(value: &impl Encode) -> aisdk_native::JsonMap {
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&encode_json(value)) else {
        return aisdk_native::JsonMap::new();
    };
    map
}

pub(crate) fn decode_object<T: Decode>(map: aisdk_native::JsonMap) -> T {
    T::decode(&Value::Object(map), &mut codec::Path::new()).expect("catalog settings decode")
}

pub(crate) fn headers_to_native(headers: &codec::JsonMap<String>) -> aisdk_native::Headers {
    headers.0.iter().cloned().collect()
}

pub(crate) fn headers_from_native(headers: aisdk_native::Headers) -> codec::JsonMap<String> {
    codec::JsonMap(headers.into_iter().collect())
}

pub(crate) fn body_to_native(body: &codec::JsonMap<Value>) -> aisdk_native::JsonMap {
    body.0.iter().cloned().collect()
}

pub(crate) fn body_from_native(body: aisdk_native::JsonMap) -> codec::JsonMap<Value> {
    codec::JsonMap(body.into_iter().collect())
}
