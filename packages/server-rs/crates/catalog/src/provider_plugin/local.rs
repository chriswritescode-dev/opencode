use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::FutureExt;
use opencode_ai::HttpSender;
use opencode_config::entry::Entry;
use opencode_contract::generated::{
    CredentialValue, IntegrationID, ModelCapabilities, ModelType, ModelTypeString2, ProviderID, ProviderInfoActivation,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::builtin::{Builtin, Services};
use crate::integration::IntegrationService;
use crate::provider::{self, ProviderService};
use crate::provider_plugin::http::{self, BufferedResponse, HttpError};
use crate::provider_plugin::{BoxFuture, PluginContext, refresh, settings};

const OPENAI_COMPATIBLE: &str = "@opencode/ai/providers/openai-compatible";

const OPENAI_COMPATIBLE_DECISIONS: &str = "@opencode/ai/providers/openai-compatible/decisions";

pub struct LocalModel {
    pub id: String,
    pub name: Option<String>,
    pub family: Option<String>,
    pub tools: Option<bool>,
    pub vision: Option<bool>,
    pub context: Option<i64>,
    pub decision: bool,
}

#[derive(Clone)]
pub struct Options {
    pub origin: Option<String>,
    pub interval: Duration,
    pub timeout: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self { origin: None, interval: Duration::from_secs(30), timeout: Duration::from_secs(15) }
    }
}

pub type Discover<D> = fn(Client) -> BoxFuture<Result<Vec<D>, DiscoverError>>;

pub struct Local<D> {
    provider_id: &'static str,
    name: &'static str,
    origin: &'static str,
    strip: &'static [&'static str],
    discovery: Mutex<HashMap<String, DiscoveryEntry<D>>>,
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    discover: Discover<D>,
    model: fn(&D) -> LocalModel,
}

struct DiscoveryEntry<D> {
    checked: tokio::time::Instant,
    api_key: Option<String>,
    models: Option<Vec<D>>,
}

impl<D> Local<D>
where
    D: Clone + PartialEq + Send + 'static,
{
    pub fn new(
        provider_id: &'static str,
        name: &'static str,
        origin: &'static str,
        strip: &'static [&'static str],
        discover: Discover<D>,
        model: fn(&D) -> LocalModel,
    ) -> Self {
        Self {
            provider_id,
            name,
            origin,
            strip,
            discovery: Mutex::new(HashMap::new()),
            locks: Mutex::new(HashMap::new()),
            discover,
            model,
        }
    }

    pub async fn register(
        &'static self,
        services: &Services,
        context: &PluginContext,
        builtin: &mut Builtin,
        options: Options,
    ) {
        let origin = options
            .origin
            .clone()
            .or_else(|| if context.location.workspace_id.is_some() { None } else { Some(self.origin.to_owned()) });
        let source = configured(&services.config.entries(), self.provider_id, origin.as_deref(), self.strip);
        let state = Arc::new(State {
            spec: self,
            http: context.http.clone(),
            integrations: services.integration.clone(),
            providers: services.provider.clone(),
            interval: options.interval,
            timeout: options.timeout,
            source: Mutex::new(source),
            loaded: Mutex::new(Loaded { models: Vec::new(), connected: false, applied: false, failing: false }),
        });

        let integration_registration = {
            let state = state.clone();
            let integration_id = IntegrationID(self.provider_id.to_owned());
            services
                .integration
                .transform(move |editor| {
                    let (found, connected) = {
                        let loaded = state.loaded.lock().expect("lock the local provider models");
                        (!loaded.models.is_empty(), loaded.connected)
                    };
                    if !found || connected {
                        return Ok(());
                    }
                    editor.remove(&integration_id);
                    Ok(())
                })
                .await
        };
        builtin.registrations.push(integration_registration);

        let provider_registration = {
            let state = state.clone();
            services
                .provider
                .transform(move |editor| {
                    let (models, connected) = {
                        let loaded = state.loaded.lock().expect("lock the local provider models");
                        (loaded.models.clone(), loaded.connected)
                    };
                    if models.is_empty() {
                        return Ok(());
                    }
                    let (base_url, api_key) = {
                        let source = state.source.lock().expect("lock the local provider source");
                        (source.base_url.clone(), source.api_key.clone())
                    };
                    let id = ProviderID(state.spec.provider_id.to_owned());
                    editor.models_set(&id, &[]);
                    editor.update(&id, |provider| {
                        provider.name = state.spec.name.to_owned();
                        provider.activation = ProviderInfoActivation::Enabled;
                        provider.package = OPENAI_COMPATIBLE.to_owned();
                        provider.settings = Some(provider::decode_object(provider::json_object(&serde_json::json!({
                            "baseURL": base_url,
                            "provider": state.spec.provider_id,
                            "apiKey": api_key.unwrap_or_default(),
                        }))));
                        if !connected {
                            provider.integration_id = None;
                        }
                    });
                    for item in &models {
                        let spec = (state.spec.model)(item);
                        let model_id = spec.id.clone();
                        editor.models_update(&id, &model_id, move |model| {
                            model.model_id = spec.id.clone();
                            model.name =
                                spec.name.clone().filter(|name| !name.is_empty()).unwrap_or_else(|| spec.id.clone());
                            model.family = spec.family.clone().filter(|family| !family.is_empty());
                            model.capabilities = ModelCapabilities {
                                tools: spec.tools.unwrap_or(false),
                                freeform: None,
                                input: if spec.vision == Some(true) {
                                    vec!["text".to_owned(), "image".to_owned()]
                                } else {
                                    vec!["text".to_owned()]
                                },
                                output: vec!["text".to_owned()],
                                temperature: None,
                            };
                            if let Some(context) = spec.context.filter(|context| *context > 0) {
                                model.limit.context = context as f64;
                            }
                            if !spec.decision {
                                return;
                            }
                            model.r#type = Some(ModelType::String2(ModelTypeString2::Decision));
                            model.package = Some(OPENAI_COMPATIBLE_DECISIONS.to_owned());
                            model.capabilities.output = vec!["decision".to_owned()];
                        });
                    }
                    Ok(())
                })
                .await
        };
        builtin.registrations.push(provider_registration);

        let poll = {
            let state = state.clone();
            let interval = state.interval;
            tokio::spawn(async move {
                loop {
                    attempt(&state).await;
                    tokio::time::sleep(interval).await;
                }
            })
        };
        builtin.tasks.push(poll);

        let switch = {
            let state = state.clone();
            refresh::on_switch(&services.bus, self.provider_id, move || {
                let state = state.clone();
                async move {
                    tokio::spawn(async move {
                        attempt(&state).await;
                    });
                }
                .boxed()
            })
        };
        builtin.tasks.push(switch);

        let observer = {
            let state = state.clone();
            opencode_config::observer::observe(&services.config, move |entries| {
                let next = configured(entries, state.spec.provider_id, origin.as_deref(), state.spec.strip);
                let changed = {
                    let mut source = state.source.lock().expect("lock the local provider source");
                    if *source == next {
                        false
                    } else {
                        *source = next;
                        true
                    }
                };
                if !changed {
                    return;
                }
                *state.loaded.lock().expect("lock the local provider models") =
                    Loaded { models: Vec::new(), connected: false, applied: false, failing: false };
                let state = state.clone();
                tokio::spawn(async move {
                    state.integrations.reload().await;
                    state.providers.reload().await;
                    attempt(&state).await;
                });
            })
        };
        builtin.observers.push(observer);
    }
}

pub struct Client {
    http: Arc<dyn HttpSender>,
    root: String,
    api_key: Option<String>,
    endpoint: Endpoint,
    timeout: Duration,
}

impl Client {
    pub fn root(&self) -> &str {
        &self.root
    }

    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }

    pub async fn get(&self, path: &str) -> Result<BufferedResponse, HttpError> {
        http::send(self.http.as_ref(), http::get(self.endpoint.path(path), self.headers()), Some(self.timeout)).await
    }

    pub async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, DiscoverError> {
        let response = self.get(path).await?;
        serde_json::from_slice(&response.body).map_err(DiscoverError::Json)
    }

    pub async fn post_json<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T, DiscoverError> {
        let request = http::json(self.endpoint.path(path), self.headers(), body);
        let response = http::send(self.http.as_ref(), request, Some(self.timeout)).await?;
        serde_json::from_slice(&response.body).map_err(DiscoverError::Json)
    }

    fn headers(&self) -> Vec<(String, String)> {
        let mut headers = vec![("accept".to_owned(), "application/json".to_owned())];
        if let Some(api_key) = &self.api_key {
            headers.push(("authorization".to_owned(), format!("Bearer {api_key}")));
        }
        headers
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DiscoverError {
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Clone, Debug, PartialEq)]
struct Source {
    base_url: String,
    api_key: Option<String>,
    explicit: bool,
    endpoint: Option<Endpoint>,
}

#[derive(Clone, Debug, PartialEq)]
struct Endpoint {
    url: url::Url,
    prefix: String,
}

impl Endpoint {
    fn path(&self, path: &str) -> String {
        let mut url = self.url.clone();
        url.set_path(&format!("{}{}", self.prefix, path));
        url.set_query(None);
        url.set_fragment(None);
        url.to_string()
    }
}

struct State<D: 'static> {
    spec: &'static Local<D>,
    http: Arc<dyn HttpSender>,
    integrations: Arc<IntegrationService>,
    providers: Arc<ProviderService>,
    interval: Duration,
    timeout: Duration,
    source: Mutex<Source>,
    loaded: Mutex<Loaded<D>>,
}

struct Loaded<D> {
    models: Vec<D>,
    connected: bool,
    applied: bool,
    failing: bool,
}

struct Discovery<D> {
    source: Source,
    connected: bool,
    models: Option<Vec<D>>,
}

async fn discover<D>(state: &Arc<State<D>>) -> Result<Option<Discovery<D>>, DiscoverError>
where
    D: Clone + PartialEq + Send + 'static,
{
    let current = state.source.lock().expect("lock the local provider source").clone();
    let Some(endpoint) = current.endpoint.clone() else {
        return Ok(None);
    };
    let root = endpoint.path("");
    let lock = {
        let mut locks = state.spec.locks.lock().expect("lock the local discovery locks");
        locks.entry(root.clone()).or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))).clone()
    };
    let _permit = lock.lock().await;
    let (connection, credential) =
        settings::active_credential(&state.integrations, &IntegrationID(state.spec.provider_id.to_owned())).await;
    let connected = connection.is_some();
    let api_key = match credential {
        Some(CredentialValue::Key(credential)) => Some(credential.key),
        _ => current.api_key.clone(),
    };
    let now = tokio::time::Instant::now();
    {
        let discovery = state.spec.discovery.lock().expect("lock the local discovery cache");
        if let Some(entry) = discovery.get(&root)
            && entry.api_key == api_key
            && now.duration_since(entry.checked) < state.interval
        {
            return Ok(Some(Discovery { source: current, connected, models: entry.models.clone() }));
        }
    }
    {
        let mut discovery = state.spec.discovery.lock().expect("lock the local discovery cache");
        let previous =
            discovery.get(&root).filter(|entry| entry.api_key == api_key).and_then(|entry| entry.models.clone());
        discovery.insert(root.clone(), DiscoveryEntry { checked: now, api_key: api_key.clone(), models: previous });
    }
    let client = Client {
        http: state.http.clone(),
        root: root.clone(),
        api_key: api_key.clone(),
        endpoint,
        timeout: state.timeout,
    };
    let models = (state.spec.discover)(client).await?;
    {
        let mut discovery = state.spec.discovery.lock().expect("lock the local discovery cache");
        discovery.insert(
            root,
            DiscoveryEntry { checked: tokio::time::Instant::now(), api_key, models: Some(models.clone()) },
        );
    }
    Ok(Some(Discovery { source: current, connected, models: Some(models) }))
}

async fn refresh<D>(state: &Arc<State<D>>) -> Result<(), DiscoverError>
where
    D: Clone + PartialEq + Send + 'static,
{
    let result = discover(state).await?;
    state.loaded.lock().expect("lock the local provider models").failing = false;
    let Some(result) = result else {
        return Ok(());
    };
    if *state.source.lock().expect("lock the local provider source") != result.source {
        return Ok(());
    }
    let Some(models) = result.models else {
        return Ok(());
    };
    let changed = {
        let mut loaded = state.loaded.lock().expect("lock the local provider models");
        let changed = !loaded.applied || loaded.connected != result.connected || loaded.models != models;
        if changed {
            loaded.models = models;
            loaded.connected = result.connected;
            loaded.applied = true;
        }
        changed
    };
    if !changed {
        return Ok(());
    }
    state.integrations.reload().await;
    state.providers.reload().await;
    Ok(())
}

async fn attempt<D>(state: &Arc<State<D>>)
where
    D: Clone + PartialEq + Send + 'static,
{
    let current = state.source.lock().expect("lock the local provider source").clone();
    let Err(error) = refresh(state).await else {
        return;
    };
    if *state.source.lock().expect("lock the local provider source") != current || !current.explicit {
        return;
    }
    {
        let mut loaded = state.loaded.lock().expect("lock the local provider models");
        if loaded.failing {
            return;
        }
        loaded.failing = true;
    }
    log::warn!("failed to discover {} models: {error}", state.spec.name);
}

fn configured(entries: &[Entry], provider_id: &str, default_origin: Option<&str>, strip: &[&str]) -> Source {
    let settings = settings::fold_settings(entries, provider_id, None);
    let base_url = match super::setting(settings.as_ref(), "baseURL") {
        Some(base_url) => base_url.to_owned(),
        None => default_origin.map(|origin| format!("{}/v1", origin.trim_end_matches('/'))).unwrap_or_default(),
    };
    let base_url = base_url.trim_end_matches('/').to_owned();
    let api_key = super::setting(settings.as_ref(), "apiKey").map(str::to_owned);
    let explicit = super::setting(settings.as_ref(), "baseURL").is_some();
    Source { endpoint: endpoint(&base_url, strip), base_url, api_key, explicit }
}

fn endpoint(base_url: &str, strip: &[&str]) -> Option<Endpoint> {
    let url = url::Url::parse(base_url).ok()?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return None;
    }
    let path = url.path().trim_end_matches('/');
    let prefix = strip.iter().find_map(|suffix| path.strip_suffix(suffix)).unwrap_or(path).to_owned();
    Some(Endpoint { url, prefix })
}
