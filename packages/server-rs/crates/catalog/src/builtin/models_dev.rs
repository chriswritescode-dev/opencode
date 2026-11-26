use std::sync::{Arc, Mutex};

use opencode_config::service::ConfigService;
use opencode_contract::codec::encode_json;
use opencode_contract::generated::{
    IntegrationEnvMethod, IntegrationEnvMethodType, IntegrationID, IntegrationKeyMethod, IntegrationKeyMethodType,
    IntegrationMethod, ModelInfo, ModelInfoStatus, ModelType, ProviderInfo,
};
use tokio::sync::broadcast;

use crate::builtin::{Builtin, Services};
use crate::integration::{Implementation, IntegrationService};
use crate::model::supports_text;
use crate::models_dev::{ModelsDevService, Snapshot};
use crate::provider::ProviderService;

const BEDROCK_PROFILE_ONLY_IDS: [&str; 19] = [
    "amazon.nova-2-lite-v1:0",
    "anthropic.claude-fable-5",
    "anthropic.claude-fable-5-1",
    "anthropic.claude-haiku-4-5-20251001-v1:0",
    "anthropic.claude-haiku-5-5",
    "anthropic.claude-opus-4-1-20250805-v1:0",
    "anthropic.claude-opus-4-5-20251101-v1:0",
    "anthropic.claude-opus-4-7",
    "anthropic.claude-opus-4-8",
    "anthropic.claude-opus-5",
    "anthropic.claude-sonnet-4-5-20250929-v1:0",
    "anthropic.claude-sonnet-5",
    "anthropic.claude-sonnet-5-5",
    "deepseek.r1-v1:0",
    "meta.llama4-maverick-17b-instruct-v1:0",
    "meta.llama4-scout-17b-instruct-v1:0",
    "mistral.pixtral-large-2502-v1:0",
    "writer.palmyra-x4-v1:0",
    "writer.palmyra-x5-v1:0",
];

pub async fn register(services: &Services, builtin: &mut Builtin) {
    let loaded: Arc<Mutex<Vec<Snapshot>>> = Arc::new(Mutex::new(Vec::new()));
    match services.models_dev.get(recommended(&services.config)).await {
        Ok(data) => *opencode_util::lock(&loaded) = snapshots(data),
        Err(error) => log::warn!("Failed to read models.dev: {error}"),
    }
    builtin.registrations.push(integration_transform(services, &loaded).await);
    builtin.registrations.push(provider_transform(services, &loaded).await);
    let subscription = services.bus.subscribe(&["models-dev.refreshed"], None);
    let updates = services.config.subscribe_updated();
    builtin.tasks.push(tokio::spawn(watch(
        services.models_dev.clone(),
        services.config.clone(),
        services.integration.clone(),
        services.provider.clone(),
        subscription,
        updates,
        loaded,
    )));
}

async fn integration_transform(
    services: &Services,
    loaded: &Arc<Mutex<Vec<Snapshot>>>,
) -> opencode_location::state::Registration {
    let loaded = loaded.clone();
    services
        .integration
        .transform(move |editor| {
            for snapshot in opencode_util::lock(&loaded).iter() {
                if snapshot.environment.is_empty() {
                    continue;
                }
                let integration_id = IntegrationID(snapshot.info.id.0.clone());
                editor.update(&integration_id, |integration| integration.name = snapshot.info.name.clone());
                editor.method_update(Implementation::Method {
                    integration_id: integration_id.clone(),
                    method: IntegrationMethod::Key(IntegrationKeyMethod {
                        r#type: IntegrationKeyMethodType::Key,
                        label: None,
                        form: None,
                    }),
                });
                editor.method_update(Implementation::Method {
                    integration_id,
                    method: IntegrationMethod::Env(IntegrationEnvMethod {
                        r#type: IntegrationEnvMethodType::Env,
                        names: snapshot.environment.clone(),
                    }),
                });
            }
            Ok(())
        })
        .await
}

async fn provider_transform(
    services: &Services,
    loaded: &Arc<Mutex<Vec<Snapshot>>>,
) -> opencode_location::state::Registration {
    let loaded = loaded.clone();
    services
        .provider
        .transform(move |editor| {
            for snapshot in opencode_util::lock(&loaded).iter() {
                let info = ProviderInfo {
                    integration_id: Some(IntegrationID(snapshot.info.id.0.clone())),
                    ..snapshot.info.clone()
                };
                editor.add(info, &snapshot.models, None);
            }
            Ok(())
        })
        .await
}

async fn watch(
    models_dev: Arc<ModelsDevService>,
    config: ConfigService,
    integration: Arc<IntegrationService>,
    provider: Arc<ProviderService>,
    mut subscription: opencode_bus::Subscription,
    mut updates: broadcast::Receiver<()>,
    loaded: Arc<Mutex<Vec<Snapshot>>>,
) {
    refresh(&models_dev, &config, &integration, &provider, &loaded).await;
    loop {
        tokio::select! {
            event = subscription.recv() => {
                if event.is_none() {
                    return;
                }
            }
            update = updates.recv() => {
                if matches!(update, Err(broadcast::error::RecvError::Closed)) {
                    return;
                }
            }
        }
        refresh(&models_dev, &config, &integration, &provider, &loaded).await;
    }
}

async fn refresh(
    models_dev: &ModelsDevService,
    config: &ConfigService,
    integration: &IntegrationService,
    provider: &ProviderService,
    loaded: &Mutex<Vec<Snapshot>>,
) {
    let data = match models_dev.get(recommended(config)).await {
        Ok(data) => data,
        Err(error) => {
            log::warn!("Failed to read models.dev: {error}");
            return;
        }
    };
    let latest = snapshots(data);
    let unchanged = {
        let current = opencode_util::lock(loaded);
        fingerprint(&latest) == fingerprint(&current)
    };
    if unchanged {
        return;
    }
    *opencode_util::lock(loaded) = latest;
    integration.reload().await;
    provider.reload().await;
}

fn recommended(config: &ConfigService) -> bool {
    config.flags().into_iter().find(|(key, _)| *key == "recommended_api").is_some_and(|(_, state)| state.enabled)
}

fn snapshots(data: Vec<Snapshot>) -> Vec<Snapshot> {
    data.into_iter()
        .filter(|snapshot| {
            snapshot.info.id.0 != "azure-cognitive-services" && snapshot.info.id.0 != "google-vertex-anthropic"
        })
        .map(|snapshot| {
            let bedrock = snapshot.info.id.0 == "amazon-bedrock";
            let models = snapshot.models.into_iter().filter(|model| keeps_model(bedrock, model)).collect();
            Snapshot { info: snapshot.info, models, voices: snapshot.voices, environment: snapshot.environment }
        })
        .collect()
}

fn keeps_model(bedrock: bool, model: &ModelInfo) -> bool {
    if model.status == ModelInfoStatus::Deprecated {
        return false;
    }
    if model.r#type.as_ref().is_some_and(is_chat) && !supports_text(model) {
        return false;
    }
    if bedrock && BEDROCK_PROFILE_ONLY_IDS.contains(&profile_id(model)) {
        return false;
    }
    true
}

fn is_chat(model_type: &ModelType) -> bool {
    match model_type {
        ModelType::String(_) => true,
        ModelType::String5(value) => value == "chat",
        _ => false,
    }
}

fn profile_id(model: &ModelInfo) -> &str {
    if model.model_id.is_empty() {
        return &model.id;
    }
    &model.model_id
}

fn fingerprint(snapshots: &[Snapshot]) -> String {
    let mut out = String::new();
    for snapshot in snapshots {
        out.push_str(&encode_json(&snapshot.info));
        out.push_str(&encode_json(&snapshot.models));
        out.push_str(&encode_json(&snapshot.environment));
    }
    out
}
