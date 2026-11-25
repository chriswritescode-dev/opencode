pub mod discovery;

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use opencode_contract::generated::{
    ConnectionInfo, CredentialValue, FormField1, FormFields1, FormStringField1, FormStringField1Type,
    IntegrationEnvMethod, IntegrationEnvMethodType, IntegrationExternalMethod, IntegrationExternalMethodType,
    IntegrationID, IntegrationKeyMethod, IntegrationKeyMethodType, IntegrationMethod, IntegrationMethodID, ModelInfo,
    ProviderID, ProviderInfo, ProviderSettings,
};
use serde_json::Value;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use self::discovery::{Deployment, Discovery, Endpoints, LoadError};
use crate::builtin::{Builtin, Services};
use crate::credential::CredentialService;
use crate::integration::IntegrationService;
use crate::model::ModelService;
use crate::provider::{self, ProviderService};
use crate::provider_plugin::settings;
use crate::provider_plugin::{PluginContext, env_value, refresh, setting, which};
use crate::shared_cache::SharedCache;

pub const ID: &str = "opencode.provider.azure";

const PROVIDER_ID: &str = "azure";

const METHOD_ID: &str = "azure-cli";

const AZURE_PACKAGE: &str = "@opencode/ai/providers/azure/";

const RESPONSES_PACKAGE: &str = "@opencode/ai/providers/azure/responses";

const RESOURCE_PATTERN: &str = "^[a-zA-Z0-9][a-zA-Z0-9-]*$";

const API_KEY_LABEL: &str = "API key";

const EXTERNAL_LABEL: &str = "Microsoft Entra ID (Azure CLI)";

const ENV_NAMES: [&str; 2] = ["AZURE_API_KEY", "AZURE_COGNITIVE_SERVICES_API_KEY"];

#[derive(Clone, Default)]
pub struct Options {
    pub endpoints: Endpoints,
    pub interval: Option<Duration>,
}

#[derive(Clone, Debug, Default)]
struct Loaded {
    connection: Option<ConnectionInfo>,
    resource: Option<String>,
    url: Option<String>,
    deployments: Option<Vec<Deployment>>,
}

struct Found {
    connection: ConnectionInfo,
    deployments: Vec<Deployment>,
}

pub async fn register(services: &Services, context: &PluginContext, builtin: &mut Builtin) {
    register_with(services, context, builtin, Options::default()).await
}

pub async fn register_with(services: &Services, context: &PluginContext, builtin: &mut Builtin, options: Options) {
    let provider_id = ProviderID(PROVIDER_ID.to_owned());
    let integration_id = IntegrationID(PROVIDER_ID.to_owned());
    let configured = settings::configured_settings(services, &provider_id).await.ok().flatten();
    let configured_resource =
        resolve_resource_name(configured.as_ref().map(|settings| settings.rest.0.as_slice()), None, &context.env)
            .is_some()
            || setting(configured.as_ref(), "baseURL").is_some();
    let available = which("az", &context.env).is_some();

    let integration_registration = services
        .integration
        .transform(move |editor| {
            editor.method_update(crate::integration::Implementation::Method {
                integration_id: IntegrationID(PROVIDER_ID.to_owned()),
                method: IntegrationMethod::Env(IntegrationEnvMethod {
                    r#type: IntegrationEnvMethodType::Env,
                    names: ENV_NAMES.iter().map(|name| (*name).to_owned()).collect(),
                }),
            });
            editor.method_update(crate::integration::Implementation::Method {
                integration_id: IntegrationID(PROVIDER_ID.to_owned()),
                method: IntegrationMethod::Key(IntegrationKeyMethod {
                    r#type: IntegrationKeyMethodType::Key,
                    label: Some(API_KEY_LABEL.to_owned()),
                    form: resource_form(configured_resource, false),
                }),
            });
            if available {
                editor.method_update(crate::integration::Implementation::Method {
                    integration_id: IntegrationID(PROVIDER_ID.to_owned()),
                    method: IntegrationMethod::External(IntegrationExternalMethod {
                        id: IntegrationMethodID(METHOD_ID.to_owned()),
                        r#type: IntegrationExternalMethodType::External,
                        label: EXTERNAL_LABEL.to_owned(),
                        form: resource_form(configured_resource, true),
                    }),
                });
            }
            Ok(())
        })
        .await;
    builtin.registrations.push(integration_registration);

    let initial = load(&services.integration, &services.credentials, &integration_id).await;
    let azure = Arc::new(Azure {
        provider: services.provider.clone(),
        model: services.model.clone(),
        integrations: services.integration.clone(),
        credentials: services.credentials.clone(),
        discovery: Arc::new(Discovery::new(
            context.http.clone(),
            context.commands.clone(),
            context.app.useragent(),
            options.endpoints.management.clone(),
        )),
        endpoints: options.endpoints.clone(),
        env: context.env.clone(),
        integration_id: integration_id.clone(),
        cache: context.shared_cache.clone(),
        interval: options.interval,
        state: Mutex::new(initial),
        gate: tokio::sync::Mutex::new(()),
    });

    let provider_registration = services
        .provider
        .transform({
            let azure = azure.clone();
            move |editor| {
                let loaded = azure.state.lock().expect("lock the Azure state").clone();
                for item in editor.list() {
                    if !azure_provider(&item.provider) {
                        continue;
                    }
                    let resource = resource_for(&item.provider, loaded.resource.as_deref(), &azure.env);
                    let websocket = responses_web_socket_capable(&item.provider);
                    if resource.is_none() && !websocket {
                        continue;
                    }
                    editor.update(&item.provider.id, |provider| {
                        let mut settings = provider.settings.as_ref().map(provider::json_object).unwrap_or_default();
                        if let Some(resource) = &resource {
                            settings.insert("resourceName".to_owned(), Value::String(resource.clone()));
                        }
                        if websocket {
                            let transport = settings
                                .get("transport")
                                .filter(|value| !value.is_null())
                                .cloned()
                                .unwrap_or_else(|| Value::String("websocket".to_owned()));
                            settings.insert("transport".to_owned(), transport);
                        }
                        if let (Some(resource), Some(base_url)) =
                            (&resource, setting(provider.settings.as_ref(), "baseURL"))
                        {
                            settings
                                .insert("baseURL".to_owned(), Value::String(expand_resource_name(base_url, resource)));
                        }
                        provider.settings = Some(provider::decode_object(settings));
                    });
                }
                if let Some(item) = editor.get(&provider_id).cloned() {
                    let models: Vec<ModelInfo> = item.models.values().cloned().collect();
                    editor.add(item.provider, &models, loaded.connection);
                }
                Ok(())
            }
        })
        .await;
    builtin.registrations.push(provider_registration);

    let model_registration = services
        .model
        .transform({
            let azure = azure.clone();
            move |editor| {
                let loaded = azure.state.lock().expect("lock the Azure state").clone();
                for item in editor.provider_list() {
                    if !azure_provider(&item.provider) {
                        continue;
                    }
                    let Some(resource) = resource_for(&item.provider, loaded.resource.as_deref(), &azure.env) else {
                        continue;
                    };
                    let provider_id = item.provider.id;
                    let model_ids: Vec<String> =
                        editor.list(Some(&provider_id)).into_iter().map(|model| model.id.clone()).collect();
                    for model_id in model_ids {
                        let (base_url, resource_name) = {
                            let Some(model) = editor.get(&provider_id, &model_id) else {
                                continue;
                            };
                            (model_base_url(model), model_resource_name(model))
                        };
                        let Some(base_url) = base_url else {
                            continue;
                        };
                        let expanded = expand_resource_name(&base_url, resource_name.as_deref().unwrap_or(&resource));
                        if expanded == base_url {
                            continue;
                        }
                        editor.update(&provider_id, &model_id, |model| {
                            let Some(settings) = model.settings.as_ref() else {
                                return;
                            };
                            let mut merged = provider::json_object(settings);
                            if merged.get("baseURL").and_then(Value::as_str).is_none() {
                                return;
                            }
                            merged.insert("baseURL".to_owned(), Value::String(expanded.clone()));
                            model.settings = Some(provider::decode_object(merged));
                        });
                    }
                }
                let Some(deployments) = loaded.deployments else {
                    return Ok(());
                };
                let provider_id = ProviderID(PROVIDER_ID.to_owned());
                let catalog: Vec<ModelInfo> =
                    editor.list(Some(&provider_id)).into_iter().map(|model| model.clone()).collect();
                let deployed = deployed_models(&deployments, &catalog);
                let deployed_ids: Vec<String> = deployed.iter().map(|(id, _)| id.clone()).collect();
                for model in &catalog {
                    if !deployed_ids.contains(&model.id) {
                        editor.remove(&provider_id, &model.id);
                    }
                }
                for (id, model) in deployed {
                    editor.update(&provider_id, &id, |draft| {
                        *draft = model.clone();
                    });
                }
                Ok(())
            }
        })
        .await;
    builtin.registrations.push(model_registration);

    let jobs = Arc::new(Jobs::default());
    let interval = options.interval.unwrap_or(crate::shared_cache::INTERVAL);

    builtin.tasks.push(tokio::spawn({
        let azure = azure.clone();
        let jobs = jobs.clone();
        async move {
            azure.rebind().await;
            let job = azure.clone();
            jobs.start(async move { job.refresh().await });
        }
    }));

    builtin.tasks.push(tokio::spawn({
        let azure = azure.clone();
        let jobs = jobs.clone();
        async move {
            let mut ticker = tokio::time::interval_at(Instant::now() + interval, interval);
            loop {
                ticker.tick().await;
                if jobs.busy() {
                    continue;
                }
                let job = azure.clone();
                jobs.start(async move { job.refresh().await });
            }
        }
    }));

    let load: refresh::Load<Option<Found>, LoadError> = Arc::new({
        let azure = azure.clone();
        move |options| {
            let azure = azure.clone();
            Box::pin(async move { Ok(azure.discover(options).await) })
        }
    });
    let apply: refresh::Apply<Option<Found>> = Arc::new({
        let azure = azure.clone();
        move |next| {
            let azure = azure.clone();
            Box::pin(async move {
                if let Some(found) = next {
                    Azure::apply(azure, found).await;
                }
            })
        }
    });
    let handle = refresh::on_switch(&services.bus, PROVIDER_ID, {
        let azure = azure.clone();
        let jobs = jobs.clone();
        move || {
            let azure = azure.clone();
            let jobs = jobs.clone();
            let load = load.clone();
            let apply = apply.clone();
            Box::pin(async move {
                azure.rebind().await;
                jobs.start(refresh::switched(load, apply));
            })
        }
    });
    builtin.tasks.push(handle);
}

struct Azure {
    provider: Arc<ProviderService>,
    model: Arc<ModelService>,
    integrations: Arc<IntegrationService>,
    credentials: Arc<CredentialService>,
    discovery: Arc<Discovery>,
    endpoints: Endpoints,
    env: crate::integration::ConfigEnv,
    integration_id: IntegrationID,
    cache: Arc<SharedCache>,
    interval: Option<Duration>,
    state: Mutex<Loaded>,
    gate: tokio::sync::Mutex<()>,
}

impl Azure {
    async fn load(&self) -> Loaded {
        load(&self.integrations, &self.credentials, &self.integration_id).await
    }

    async fn rebind(&self) {
        let _permit = self.gate.lock().await;
        let next = self.load().await;
        let changed = {
            let mut state = self.state.lock().expect("lock the Azure state");
            if connection_key(state.connection.as_ref()) == connection_key(next.connection.as_ref())
                && state.resource == next.resource
            {
                false
            } else {
                state.connection = next.connection;
                state.resource = next.resource;
                state.url = None;
                state.deployments = None;
                true
            }
        };
        if changed {
            self.provider.reload().await;
        }
    }

    async fn discover(&self, options: refresh::Options) -> Option<Found> {
        let (connection, resource, url_before) = {
            let state = self.state.lock().expect("lock the Azure state");
            (state.connection.clone(), state.resource.clone(), state.url.clone())
        };
        let settings = self
            .provider
            .get(&ProviderID(PROVIDER_ID.to_owned()))
            .await
            .ok()
            .flatten()
            .and_then(|provider| provider.settings);
        let name = resource.clone().or_else(|| {
            resolve_resource_name(settings.as_ref().map(|settings| settings.rest.0.as_slice()), None, &self.env)
        });
        let url = match (&connection, &name) {
            (Some(_), Some(name)) if valid_resource(name) && setting(settings.as_ref(), "baseURL").is_none() => {
                Some(format!("{}/deployments?api-version=2022-12-01", (self.endpoints.resource)(name)))
            }
            _ => None,
        };
        {
            let state = self.state.lock().expect("lock the Azure state");
            if connection_key(state.connection.as_ref()) != connection_key(connection.as_ref()) {
                return None;
            }
        }
        if url_before != url {
            let reload = {
                let mut state = self.state.lock().expect("lock the Azure state");
                state.url = url.clone();
                if state.deployments.is_some() {
                    state.deployments = None;
                    true
                } else {
                    false
                }
            };
            if reload {
                let _ = self.model.reload().await;
            }
        }
        let connection = connection?;
        let name = name?;
        let url = url?;
        let credential = self.integrations.connection().resolve(&connection).await.ok().flatten();
        if !credential.as_ref().is_some_and(usable) {
            return None;
        }
        let credential = credential.expect("the credential is usable");
        let key = deployments_key(&connection, &credential, &url);
        let discovery = self.discovery.clone();
        let integrations = self.integrations.clone();
        let load_connection = connection.clone();
        let load_name = name.clone();
        let load_url = url.clone();
        let found = self
            .cache
            .get(&key, options.interval.or(self.interval), options.force, move || {
                let discovery = discovery.clone();
                let integrations = integrations.clone();
                let connection = load_connection.clone();
                let name = load_name.clone();
                let url = load_url.clone();
                async move {
                    let current = integrations
                        .connection()
                        .resolve(&connection)
                        .await
                        .map_err(|error| LoadError::Message(error.to_string()))?;
                    let Some(current) = current.filter(usable) else {
                        return Err(LoadError::MissingCredential);
                    };
                    let mut found = discovery.deployments(&url, &name, &current).await?;
                    found.sort_by(|left, right| left.name.cmp(&right.name));
                    Ok(found)
                }
            })
            .await
            .ok()?;
        Some(Found { connection, deployments: found })
    }

    async fn publish(self: &Arc<Self>, found: Found) {
        let _permit = self.gate.lock().await;
        {
            let state = self.state.lock().expect("lock the Azure state");
            if connection_key(state.connection.as_ref()) != connection_key(Some(&found.connection)) {
                return;
            }
            if state.deployments.as_ref() == Some(&found.deployments) {
                return;
            }
        }
        let active = self.integrations.connection().active(&self.integration_id).await.ok().flatten();
        if connection_key(active.as_ref()) != connection_key(Some(&found.connection)) {
            return;
        }
        let catalog: HashMap<String, String> = self
            .provider
            .snapshot()
            .await
            .ok()
            .and_then(|snapshot| {
                snapshot
                    .records
                    .get(PROVIDER_ID)
                    .map(|definition| definition.models.keys().map(|id| (id.to_lowercase(), id.clone())).collect())
            })
            .unwrap_or_default();
        let unmatched: Vec<&str> = found
            .deployments
            .iter()
            .filter(|deployment| catalog_model(&catalog, deployment).is_none())
            .map(|deployment| deployment.name.as_str())
            .collect();
        if !unmatched.is_empty() {
            log::warn!(
                "Azure deployments of models outside the catalog need explicit configuration: {}",
                unmatched.join(", ")
            );
        }
        self.state.lock().expect("lock the Azure state").deployments = Some(found.deployments);
        let _ = self.model.reload().await;
    }

    async fn refresh(self: Arc<Self>) {
        if let Some(found) = self.discover(refresh::Options::default()).await {
            Self::apply(self, found).await;
        }
    }

    async fn apply(azure: Arc<Self>, found: Found) {
        let job = tokio::spawn(async move { azure.publish(found).await });
        let _ = job.await;
    }
}

struct Jobs {
    current: Mutex<Option<JoinHandle<()>>>,
}

impl Default for Jobs {
    fn default() -> Self {
        Self { current: Mutex::new(None) }
    }
}

impl Jobs {
    fn start(&self, future: impl Future<Output = ()> + Send + 'static) {
        let previous = self.current.lock().expect("lock the Azure discovery job").take();
        if let Some(previous) = previous {
            previous.abort();
        }
        *self.current.lock().expect("lock the Azure discovery job") = Some(tokio::spawn(future));
    }

    fn busy(&self) -> bool {
        self.current.lock().expect("lock the Azure discovery job").as_ref().is_some_and(|handle| !handle.is_finished())
    }
}

impl Drop for Jobs {
    fn drop(&mut self) {
        if let Ok(mut current) = self.current.lock()
            && let Some(handle) = current.take()
        {
            handle.abort();
        }
    }
}

async fn load(
    integrations: &IntegrationService,
    credentials: &CredentialService,
    integration_id: &IntegrationID,
) -> Loaded {
    let (connection, credential) = settings::read_stored_credential(integrations, credentials, integration_id).await;
    Loaded { connection, resource: credential_resource(credential.as_ref()), url: None, deployments: None }
}

fn resource_form(configured: bool, external: bool) -> Option<FormFields1> {
    if configured {
        return None;
    }
    let field = if external {
        FormStringField1 {
            key: "resourceName".to_owned(),
            title: Some("Azure resource".to_owned()),
            description: Some("Enter the name of your Azure OpenAI or AI Services resource.".to_owned()),
            required: Some(true),
            hidden: None,
            when: None,
            r#type: FormStringField1Type::String,
            format: None,
            min_length: None,
            max_length: None,
            pattern: Some(RESOURCE_PATTERN.to_owned()),
            placeholder: Some("e.g. my-models".to_owned()),
            default: None,
            options: None,
            custom: None,
        }
    } else {
        FormStringField1 {
            key: "resourceName".to_owned(),
            title: Some("Enter Azure Resource Name".to_owned()),
            description: None,
            required: Some(true),
            hidden: None,
            when: None,
            r#type: FormStringField1Type::String,
            format: None,
            min_length: None,
            max_length: None,
            pattern: None,
            placeholder: Some("e.g. my-models".to_owned()),
            default: None,
            options: None,
            custom: None,
        }
    };
    Some(FormFields1(vec![FormField1::String(field)]))
}

fn azure_provider(provider: &ProviderInfo) -> bool {
    provider.id.0 == PROVIDER_ID || provider.package.starts_with(AZURE_PACKAGE)
}

fn resource_for(provider: &ProviderInfo, loaded: Option<&str>, env: &crate::integration::ConfigEnv) -> Option<String> {
    if provider.id.0 == PROVIDER_ID {
        loaded.map(str::to_owned).or_else(|| {
            resolve_resource_name(provider.settings.as_ref().map(|settings| settings.rest.0.as_slice()), None, env)
        })
    } else {
        resolve_resource_name(provider.settings.as_ref().map(|settings| settings.rest.0.as_slice()), loaded, env)
    }
}

fn resolve_resource_name(
    settings: Option<&[(String, Value)]>,
    fallback: Option<&str>,
    env: &crate::integration::ConfigEnv,
) -> Option<String> {
    let configured = settings.and_then(|settings| {
        settings.iter().find(|(name, _)| name == "resourceName").and_then(|(_, value)| value.as_str())
    });
    if let Some(configured) = configured.filter(|configured| !configured.trim().is_empty()) {
        return Some(configured.to_owned());
    }
    if let Some(fallback) = fallback {
        return Some(fallback.to_owned());
    }
    env_value(env, "AZURE_RESOURCE_NAME").or_else(|| env_value(env, "AZURE_COGNITIVE_SERVICES_RESOURCE_NAME"))
}

fn expand_resource_name(base_url: &str, resource_name: &str) -> String {
    base_url
        .replace("${AZURE_RESOURCE_NAME}", resource_name)
        .replace("${AZURE_COGNITIVE_SERVICES_RESOURCE_NAME}", resource_name)
}

fn valid_resource(resource: &str) -> bool {
    let mut chars = resource.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_alphanumeric())
        && chars.all(|character| character.is_ascii_alphanumeric() || character == '-')
}

fn responses_web_socket_capable(provider: &ProviderInfo) -> bool {
    if provider.package != RESPONSES_PACKAGE {
        return false;
    }
    let settings = provider.settings.as_ref();
    if setting_value(settings, "useDeploymentBasedUrls") == Some(&Value::Bool(true)) {
        return false;
    }
    if let Some(api_version) = setting_value(settings, "apiVersion")
        && api_version.as_str() != Some("v1")
    {
        return false;
    }
    match setting(settings, "baseURL") {
        None => true,
        Some(base_url) => azure_openai_host(base_url),
    }
}

fn setting_value<'a>(settings: Option<&'a ProviderSettings>, key: &str) -> Option<&'a Value> {
    settings?.rest.0.iter().find(|(name, _)| name == key).map(|(_, value)| value)
}

fn azure_openai_host(base_url: &str) -> bool {
    let lower = base_url.to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix("https://") else {
        return false;
    };
    let (host, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, ""),
    };
    let suffix = ".openai.azure.com";
    host.len() > suffix.len() && host.ends_with(suffix) && (path.is_empty() || path.starts_with('/'))
}

fn model_base_url(model: &ModelInfo) -> Option<String> {
    model.settings.as_ref()?.rest.0.iter().find(|(name, _)| name == "baseURL")?.1.as_str().map(str::to_owned)
}

fn model_resource_name(model: &ModelInfo) -> Option<String> {
    model.settings.as_ref()?.rest.0.iter().find(|(name, _)| name == "resourceName")?.1.as_str().map(str::to_owned)
}

fn deployed_models(deployments: &[Deployment], catalog: &[ModelInfo]) -> Vec<(String, ModelInfo)> {
    let models: HashMap<String, &ModelInfo> = catalog.iter().map(|model| (model.id.to_lowercase(), model)).collect();
    let mut deployed: Vec<(String, ModelInfo)> = Vec::new();
    for deployment in deployments {
        let Some(model) = catalog_model(&models, deployment).copied() else {
            continue;
        };
        let id = deployment.name.to_lowercase();
        let name = if id == model.id.to_lowercase() {
            model.name.clone()
        } else {
            format!("{} ({})", model.name, deployment.name)
        };
        let info = ModelInfo { id: id.clone(), model_id: deployment.name.clone(), name, ..model.clone() };
        if let Some(existing) = deployed.iter_mut().find(|(existing, _)| *existing == id) {
            existing.1 = info;
        } else {
            deployed.push((id, info));
        }
    }
    deployed
}

fn catalog_model<'a, T>(models: &'a HashMap<String, T>, deployment: &Deployment) -> Option<&'a T> {
    models.get(&deployment.model.to_lowercase()).or_else(|| models.get(&deployment.name.to_lowercase()))
}

fn usable(credential: &CredentialValue) -> bool {
    match credential {
        CredentialValue::Key(_) => true,
        CredentialValue::External(credential) => credential.method_id.0 == METHOD_ID,
        CredentialValue::Oauth(_) => false,
    }
}

fn credential_resource(credential: Option<&CredentialValue>) -> Option<String> {
    let metadata = match credential? {
        CredentialValue::Key(credential) => credential.metadata.as_ref(),
        CredentialValue::External(credential) if credential.method_id.0 == METHOD_ID => credential.metadata.as_ref(),
        _ => return None,
    };
    let resource = metadata?
        .0
        .iter()
        .find(|(name, _)| name == "resourceName")?
        .1
        .as_str()
        .filter(|resource| !resource.trim().is_empty())?;
    Some(resource.to_owned())
}

fn connection_key(connection: Option<&ConnectionInfo>) -> Option<String> {
    connection.map(crate::integration::key)
}

fn deployments_key(connection: &ConnectionInfo, credential: &CredentialValue, url: &str) -> String {
    let access = match credential {
        CredentialValue::Key(credential) => {
            format!(":{}", crate::shared_cache::fingerprint(&credential.key))
        }
        _ => String::new(),
    };
    format!("azure:{}{}:{}", crate::integration::key(connection), access, url)
}
