use std::sync::{Arc, Mutex};

use opencode_config::entry::Entry;
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    ConfigProviderSettings, ConnectionInfo, CredentialValue, IntegrationID, ProviderID, ProviderSettings,
};

use crate::aisdk_native;
use crate::builtin::{Builtin, Services};
use crate::credential::CredentialService;
use crate::integration::IntegrationService;
use crate::provider::{self, ProviderError, ProviderService};
use crate::provider_plugin::refresh;

type Selector = dyn Fn(&CredentialValue) -> ProviderSettings + Send + Sync;

pub async fn configured_settings(
    services: &Services,
    id: &ProviderID,
) -> Result<Option<ProviderSettings>, ProviderError> {
    let current = services.provider.get(id).await?.and_then(|provider| provider.settings);
    Ok(fold_settings(&services.config.entries(), &id.0, current))
}

pub fn fold_settings(entries: &[Entry], id: &str, initial: Option<ProviderSettings>) -> Option<ProviderSettings> {
    entries.iter().fold(initial, |settings, entry| {
        let Entry::Document(document) = entry else {
            return settings;
        };
        let Some(overlay) = document
            .info
            .providers
            .as_ref()
            .and_then(|providers| providers.0.iter().find(|(key, _)| key.as_str() == id))
            .and_then(|(_, provider)| provider.settings.as_ref())
        else {
            return settings;
        };
        Some(merge_provider_settings(settings.as_ref(), overlay))
    })
}

pub async fn active_credential(
    integrations: &IntegrationService,
    integration_id: &IntegrationID,
) -> (Option<ConnectionInfo>, Option<CredentialValue>) {
    let Ok(connection) = integrations.connection().active(integration_id).await else {
        return (None, None);
    };
    let credential = match connection.as_ref() {
        Some(connection) => integrations.connection().resolve(connection).await.ok().flatten(),
        None => None,
    };
    (connection, credential)
}

pub async fn stored_credential(
    services: &Services,
    integration_id: &IntegrationID,
) -> (Option<ConnectionInfo>, Option<CredentialValue>) {
    read_stored_credential(&services.integration, &services.credentials, integration_id).await
}

pub async fn credential_settings(
    services: &Services,
    builtin: &mut Builtin,
    integration_id: &IntegrationID,
    select: impl Fn(&CredentialValue) -> ProviderSettings + Send + Sync + 'static,
) -> Arc<Mutex<ProviderSettings>> {
    let select: Arc<Selector> = Arc::new(select);
    let (_, credential) = stored_credential(services, integration_id).await;
    let initial = credential.as_ref().map(|credential| select(credential)).unwrap_or_else(empty_settings);
    let settings = Arc::new(Mutex::new(initial));
    let integrations = services.integration.clone();
    let credentials = services.credentials.clone();
    let providers = services.provider.clone();
    let integration = integration_id.clone();
    let cell = settings.clone();
    let handle = refresh::on_switch(&services.bus, &integration_id.0, move || {
        let integrations = integrations.clone();
        let credentials = credentials.clone();
        let providers = providers.clone();
        let select = select.clone();
        let cell = cell.clone();
        let integration = integration.clone();
        Box::pin(async move {
            switch_settings(integrations, credentials, providers, select, cell, integration).await;
        })
    });
    builtin.tasks.push(handle);
    settings
}

pub fn metadata_settings(metadata: Option<&JsonMap<serde_json::Value>>, keys: &[&str]) -> ProviderSettings {
    let rest = keys
        .iter()
        .filter_map(|key| {
            let value =
                metadata?.0.iter().find(|(name, _)| name == key)?.1.as_str().filter(|value| !value.is_empty())?;
            Some(((*key).to_owned(), serde_json::Value::String(value.to_owned())))
        })
        .collect();
    ProviderSettings { rest: JsonMap(rest), ..empty_settings() }
}

async fn switch_settings(
    integrations: Arc<IntegrationService>,
    credentials: Arc<CredentialService>,
    providers: Arc<ProviderService>,
    select: Arc<Selector>,
    settings: Arc<Mutex<ProviderSettings>>,
    integration_id: IntegrationID,
) {
    let (_, credential) = read_stored_credential(&integrations, &credentials, &integration_id).await;
    let next = credential.as_ref().map(|credential| select(credential)).unwrap_or_else(empty_settings);
    {
        let mut current = settings.lock().expect("lock provider settings");
        if *current == next {
            return;
        }
        *current = next;
    }
    providers.reload().await;
}

pub(crate) async fn read_stored_credential(
    integrations: &IntegrationService,
    credentials: &CredentialService,
    integration_id: &IntegrationID,
) -> (Option<ConnectionInfo>, Option<CredentialValue>) {
    let Ok(connection) = integrations.connection().active(integration_id).await else {
        return (None, None);
    };
    let credential = match connection.as_ref() {
        Some(ConnectionInfo::Credential(connection)) => {
            credentials.get(&connection.id).ok().flatten().map(|stored| stored.value)
        }
        _ => None,
    };
    (connection, credential)
}

fn merge_provider_settings(base: Option<&ProviderSettings>, overlay: &ConfigProviderSettings) -> ProviderSettings {
    let base = base.map(provider::json_object);
    let overlay = provider::json_object(overlay);
    provider::decode_object(
        aisdk_native::merge_overlay(base.as_ref(), Some(&overlay)).expect("an overlay merges into a record"),
    )
    .unwrap_or_else(empty_settings)
}

pub(crate) fn empty_settings() -> ProviderSettings {
    ProviderSettings {
        timeout: None,
        header_timeout: None,
        chunk_timeout: None,
        compaction: None,
        cache: None,
        transport: None,
        encoding: None,
        tls: None,
        rest: JsonMap::default(),
    }
}
