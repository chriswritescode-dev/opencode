use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use opencode_contract::generated::{
    ConnectionInfo, CredentialValue, FormField1, FormFields1, FormOption, FormStringField1, FormStringField1Type,
    IntegrationEnvMethod, IntegrationEnvMethodType, IntegrationExternalMethod, IntegrationExternalMethodType,
    IntegrationID, IntegrationMethod, IntegrationMethodID, ProviderID, ProviderInfo, ProviderInfoActivation,
    ProviderSettings,
};
use serde_json::{Map, Value};

use crate::builtin::{Builtin, Services};
use crate::credential::CredentialService;
use crate::integration::{ConfigEnv, Implementation, IntegrationService};
use crate::provider;
use crate::provider_plugin::settings;

pub const ID: &str = "opencode.provider.google.vertex";

const PROVIDER_ID: &str = "google-vertex";

const ADC_METHOD: &str = "google-adc";

const VERTEX_PACKAGE: &str = "@opencode/ai/providers/google-vertex";

const OPENAI_COMPATIBLE_PACKAGE: &str = "@opencode/ai/providers/openai-compatible";

type Settings = Map<String, Value>;

pub async fn register(services: &Services, context: &super::PluginContext, builtin: &mut Builtin) {
    let integration_id = IntegrationID(PROVIDER_ID.to_owned());
    let configured = settings::configured_settings(services, &ProviderID(PROVIDER_ID.to_owned())).await.ok().flatten();
    let discovery = discover(&context.env, configured.as_ref());
    let projects = discovery.projects;
    let locations = discovery.locations;

    let method_registration = {
        let projects = projects.clone();
        let locations = locations.clone();
        services
            .integration
            .transform(move |editor| {
                let integration_id = IntegrationID(PROVIDER_ID.to_owned());
                editor.method_update(Implementation::Method {
                    integration_id: integration_id.clone(),
                    method: IntegrationMethod::Env(IntegrationEnvMethod {
                        r#type: IntegrationEnvMethodType::Env,
                        names: vec!["GOOGLE_VERTEX_API_KEY".to_owned()],
                    }),
                });
                editor.method_update(Implementation::Method {
                    integration_id,
                    method: IntegrationMethod::External(IntegrationExternalMethod {
                        id: IntegrationMethodID(ADC_METHOD.to_owned()),
                        r#type: IntegrationExternalMethodType::External,
                        label: "Google Cloud credentials (gcloud auth or environment)".to_owned(),
                        form: Some(FormFields1(vec![
                            FormField1::String(project_field(&projects)),
                            FormField1::String(location_field(&locations)),
                        ])),
                    }),
                });
                Ok(())
            })
            .await
    };
    builtin.registrations.push(method_registration);

    let selected =
        Arc::new(Mutex::new(load(services.integration.as_ref(), services.credentials.as_ref(), &integration_id).await));

    let provider_registration = {
        let selected = selected.clone();
        let env = context.env.clone();
        services
            .provider
            .transform(move |editor| {
                let selected = selected.lock().expect("lock the selected Google Vertex settings").clone();
                for item in editor.list() {
                    if !is_vertex(&item.provider) {
                        continue;
                    }
                    let settings = settings_for(&item.provider, &selected);
                    let project = resolve_project(&settings, &env);
                    let location = resolve_location(&settings, &env).unwrap_or_else(|| "global".to_owned());
                    editor.update(&item.provider.id, |provider| {
                        if project.as_deref().is_some_and(|project| !project.is_empty())
                            && provider.activation == ProviderInfoActivation::Auto
                        {
                            provider.activation = ProviderInfoActivation::Enabled;
                        }
                        let mut merged = settings_for(provider, &selected);
                        if let Some(project) = project.as_deref().filter(|project| !project.is_empty()) {
                            merged.insert("project".to_owned(), Value::String(project.to_owned()));
                        }
                        merged.insert("location".to_owned(), Value::String(location.clone()));
                        if let Some(base_url) = super::setting(provider.settings.as_ref(), "baseURL") {
                            merged.insert(
                                "baseURL".to_owned(),
                                Value::String(replace_vertex_vars(base_url, project.as_deref(), &location)),
                            );
                        }
                        provider.settings = Some(provider::decode_object(merged));
                    });
                }
                Ok(())
            })
            .await
    };
    builtin.registrations.push(provider_registration);

    let model_registration = {
        let selected = selected.clone();
        let env = context.env.clone();
        services
            .model
            .transform(move |editor| {
                let selected = selected.lock().expect("lock the selected Google Vertex settings").clone();
                for item in editor.provider_list() {
                    if !is_vertex(&item.provider) {
                        continue;
                    }
                    let settings = settings_for(&item.provider, &selected);
                    let project = resolve_project(&settings, &env);
                    let location = resolve_location(&settings, &env).unwrap_or_else(|| "global".to_owned());
                    let provider_id = item.provider.id;
                    let model_ids: Vec<String> =
                        editor.list(Some(&provider_id)).into_iter().map(|model| model.id.clone()).collect();
                    for model_id in model_ids {
                        let Some(base_url) = editor
                            .get(&provider_id, &model_id)
                            .and_then(|model| model_base_url(model))
                            .map(str::to_owned)
                        else {
                            continue;
                        };
                        let base_url = replace_vertex_vars(&base_url, project.as_deref(), &location);
                        editor.update(&provider_id, &model_id, |model| {
                            let mut merged = model.settings.as_ref().map(provider::json_object).unwrap_or_default();
                            merged.insert("baseURL".to_owned(), Value::String(base_url.clone()));
                            model.settings = Some(provider::decode_object(merged));
                        });
                    }
                }
                Ok(())
            })
            .await
    };
    builtin.registrations.push(model_registration);

    let task = {
        let integrations = services.integration.clone();
        let credentials = services.credentials.clone();
        let providers = services.provider.clone();
        let models = services.model.clone();
        let selected = selected.clone();
        let mut subscription = services.bus.subscribe(&["credential.updated", "credential.switched"], None);
        tokio::spawn(async move {
            while let Some(event) = subscription.recv().await {
                if event.data().get("integrationID").and_then(Value::as_str) != Some(PROVIDER_ID) {
                    continue;
                }
                let next = load(&integrations, &credentials, &IntegrationID(PROVIDER_ID.to_owned())).await;
                let changed = {
                    let mut current = selected.lock().expect("lock the selected Google Vertex settings");
                    if *current == next {
                        false
                    } else {
                        *current = next;
                        true
                    }
                };
                if !changed {
                    continue;
                }
                providers.reload().await;
                if let Err(error) = models.reload().await {
                    log::warn!("the Google Vertex model reload failed: {error}");
                }
            }
        })
    };
    builtin.tasks.push(task);
}

fn model_base_url(model: &opencode_contract::generated::ModelInfo) -> Option<&str> {
    model.settings.as_ref()?.rest.0.iter().find(|(key, _)| key == "baseURL")?.1.as_str()
}

fn is_vertex(provider: &ProviderInfo) -> bool {
    provider.package.starts_with(VERTEX_PACKAGE)
        || (provider.id.0 == PROVIDER_ID && provider.package == OPENAI_COMPATIBLE_PACKAGE)
}

fn settings_for(provider: &ProviderInfo, selected: &Settings) -> Settings {
    let mut settings = provider.settings.as_ref().map(provider::json_object).unwrap_or_default();
    let integration = provider.integration_id.as_ref().map(|id| id.0.as_str()).unwrap_or(provider.id.0.as_str());
    if integration == PROVIDER_ID {
        settings.extend(selected.clone());
    }
    settings
}

fn resolve_project(settings: &Settings, env: &ConfigEnv) -> Option<String> {
    settings.get("project").and_then(Value::as_str).map(str::to_owned).or_else(|| {
        ["GOOGLE_VERTEX_PROJECT", "GOOGLE_CLOUD_PROJECT", "GCP_PROJECT", "GCLOUD_PROJECT"]
            .into_iter()
            .find_map(|name| environment(env, name))
    })
}

fn resolve_location(settings: &Settings, env: &ConfigEnv) -> Option<String> {
    settings.get("location").and_then(Value::as_str).map(str::to_owned).or_else(|| {
        ["GOOGLE_VERTEX_LOCATION", "GOOGLE_CLOUD_LOCATION", "VERTEX_LOCATION"]
            .into_iter()
            .find_map(|name| environment(env, name))
    })
}

fn environment(env: &ConfigEnv, name: &str) -> Option<String> {
    (env)(name).map(|value| value.to_string_lossy().into_owned())
}

fn vertex_endpoint(location: &str) -> String {
    if location == "global" {
        return "aiplatform.googleapis.com".to_owned();
    }
    if location == "eu" || location == "us" {
        return format!("aiplatform.{location}.rep.googleapis.com");
    }
    format!("{location}-aiplatform.googleapis.com")
}

fn replace_vertex_vars(value: &str, project: Option<&str>, location: &str) -> String {
    value
        .replace("${GOOGLE_VERTEX_PROJECT}", project.unwrap_or("${GOOGLE_VERTEX_PROJECT}"))
        .replace("${GOOGLE_VERTEX_LOCATION}", location)
        .replace("${GOOGLE_VERTEX_ENDPOINT}", &vertex_endpoint(location))
}

async fn load(
    integrations: &IntegrationService,
    credentials: &CredentialService,
    integration_id: &IntegrationID,
) -> Settings {
    let connection = integrations.connection().active(integration_id).await.ok().flatten();
    let Some(ConnectionInfo::Credential(connection)) = connection else {
        return Settings::new();
    };
    let Ok(Some(stored)) = credentials.get(&connection.id) else {
        return Settings::new();
    };
    match stored.value {
        CredentialValue::External(external) if external.method_id.0 == ADC_METHOD => {
            provider::json_object(&settings::metadata_settings(external.metadata.as_ref(), &["project", "location"]))
        }
        _ => Settings::new(),
    }
}

struct Discovery {
    projects: Vec<String>,
    locations: Vec<String>,
}

fn discover(env: &ConfigEnv, configured: Option<&ProviderSettings>) -> Discovery {
    let configured = configured.map(provider::json_object).unwrap_or_default();
    let gcloud = gcloud_directory(env);
    let active = gcloud.as_deref().map(active_config);
    let configs = gcloud
        .as_deref()
        .zip(active.as_deref())
        .map(|(gcloud, active)| config_names(gcloud, active))
        .unwrap_or_default();
    let contents = gcloud
        .as_deref()
        .map(|gcloud| {
            configs.iter().map(|name| read_file(&gcloud.join("configurations").join(name))).collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let quota_project = gcloud
        .as_deref()
        .and_then(|gcloud| read_file(&gcloud.join("application_default_credentials.json")))
        .and_then(|content| adc_project(&content));

    let mut projects = Vec::new();
    push_unique(&mut projects, resolve_project(&configured, env));
    for content in contents.iter().flatten() {
        push_unique(&mut projects, config_values(content, "project").into_iter().next().map(str::to_owned));
    }
    push_unique(&mut projects, quota_project);

    let mut locations = Vec::new();
    push_unique(&mut locations, resolve_location(&configured, env));
    for location in ["global", "us", "eu"] {
        push_unique(&mut locations, Some(location.to_owned()));
    }
    for content in contents.iter().flatten() {
        for region in config_values(content, "region") {
            push_unique(&mut locations, Some(region.to_owned()));
        }
    }

    Discovery { projects, locations }
}

fn push_unique(values: &mut Vec<String>, value: Option<String>) {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return;
    };
    if !values.contains(&value) {
        values.push(value);
    }
}

fn gcloud_directory(env: &ConfigEnv) -> Option<PathBuf> {
    let home = (env)("HOME")
        .or_else(|| (env)("USERPROFILE"))
        .map(|value| value.to_string_lossy().into_owned())
        .filter(|value| !value.is_empty());
    if let Some(config) = environment(env, "CLOUDSDK_CONFIG").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(opencode_ai::providers::google_adc::expand_cloud_sdk_config(
            &config,
            home.as_deref(),
        )));
    }
    if cfg!(windows)
        && let Some(appdata) = environment(env, "APPDATA").filter(|value| !value.is_empty())
    {
        return Some(PathBuf::from(appdata).join("gcloud"));
    }
    Some(PathBuf::from(home?).join(".config").join("gcloud"))
}

fn active_config(gcloud: &Path) -> String {
    read_file(&gcloud.join("active_config"))
        .map(|content| content.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "default".to_owned())
}

fn config_names(gcloud: &Path, active: &str) -> Vec<String> {
    let continuation = format!("config_{active}");
    let mut names: Vec<String> = std::fs::read_dir(gcloud.join("configurations"))
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .filter(|name| name.starts_with("config_"))
        .collect();
    names.sort();
    names.sort_by_key(|name| name != &continuation);
    names
}

fn config_values<'a>(content: &'a str, key: &str) -> Vec<&'a str> {
    content
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix(key)?.trim_start();
            let rest = rest.strip_prefix('=')?.trim_start();
            rest.split_whitespace().next()
        })
        .collect()
}

fn adc_project(content: &str) -> Option<String> {
    let value: Value = serde_json::from_str(content).ok()?;
    value.get("quota_project_id").and_then(Value::as_str).map(str::to_owned)
}

fn read_file(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn project_field(projects: &[String]) -> FormStringField1 {
    FormStringField1 {
        key: "project".to_owned(),
        title: Some("Google Cloud project".to_owned()),
        description: Some(if projects.is_empty() {
            "No projects found in your gcloud configuration.".to_owned()
        } else {
            format!(
                "Found {} project{} in your gcloud configuration.",
                projects.len(),
                if projects.len() == 1 { "" } else { "s" }
            )
        }),
        required: Some(true),
        hidden: None,
        when: None,
        r#type: FormStringField1Type::String,
        format: None,
        min_length: Some(1.0),
        max_length: None,
        pattern: Some("\\S".to_owned()),
        placeholder: Some("Project ID".to_owned()),
        default: projects.first().cloned(),
        options: Some(projects.iter().map(|project| option(project)).collect()),
        custom: Some(true),
    }
}

fn location_field(locations: &[String]) -> FormStringField1 {
    FormStringField1 {
        key: "location".to_owned(),
        title: Some("Location".to_owned()),
        description: Some(
            "global serves the most models. Use us, eu, or a single region for data residency or regional quota."
                .to_owned(),
        ),
        required: Some(true),
        hidden: None,
        when: None,
        r#type: FormStringField1Type::String,
        format: None,
        min_length: Some(1.0),
        max_length: None,
        pattern: Some("\\S".to_owned()),
        placeholder: Some("Location".to_owned()),
        default: locations.first().cloned(),
        options: Some(locations.iter().map(|location| option(location)).collect()),
        custom: Some(true),
    }
}

fn option(value: &str) -> FormOption {
    FormOption { value: value.to_owned(), label: value.to_owned(), description: None }
}
