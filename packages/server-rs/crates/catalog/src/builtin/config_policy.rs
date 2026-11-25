use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use opencode_config::entry::{Entry, Origin};
use opencode_config::observer::observe;
use opencode_config::policy::{self, Action, Effect};
use opencode_config::seams;
use opencode_contract::generated::{CredentialValue, ProviderInfoActivation};

use crate::builtin::{Builtin, Services};
use crate::credential::CredentialService;
use crate::provider::ProviderService;

const SUBSCRIBED: [&str; 2] = ["credential.updated", "credential.switched"];

pub async fn register(services: &Services, builtin: &mut Builtin) {
    let entries = Arc::new(RwLock::new(services.config.entries()));
    builtin.observers.push(observe(&services.config, {
        let entries = entries.clone();
        let provider = services.provider.clone();
        move |fresh| {
            *entries.write().expect("the config policy entry lock is never poisoned") = fresh.to_vec();
            let provider = provider.clone();
            tokio::spawn(async move { provider.reload().await });
        }
    }));

    let subscription = services.bus.subscribe(&SUBSCRIBED, None);
    let accounts = Arc::new(RwLock::new(read_accounts(&services.credentials)));
    builtin.tasks.push(tokio::spawn(subscribe_credentials(
        services.credentials.clone(),
        services.provider.clone(),
        subscription,
        accounts.clone(),
    )));

    let transform = services
        .provider
        .transform({
            let entries = entries.clone();
            let accounts = accounts.clone();
            move |editor| {
                let entries = entries.read().expect("the config policy entry lock is never poisoned");
                let statements = policy::statements(&entries);
                let suppliers = suppliers(&entries);
                let accounts = accounts.read().expect("the config policy account lock is never poisoned");
                let mut denied = Vec::new();
                for record in editor.list() {
                    let integration_id = suppliers
                        .get(&record.provider.id.0)
                        .cloned()
                        .or_else(|| record.provider.integration_id.as_ref().map(|id| id.0.clone()))
                        .unwrap_or_else(|| record.provider.id.0.clone());
                    let account = accounts.get(&integration_id).and_then(|account| account.as_deref());
                    if policy::decision(&statements, Action::ProviderUse, &record.provider.id.0) == Effect::Allow
                        && policy::decision(
                            &statements,
                            Action::IntegrationUse,
                            &policy::integration_resource(&integration_id, account),
                        ) == Effect::Allow
                    {
                        continue;
                    }
                    editor.remove(&record.provider.id);
                    if record.provider.activation == ProviderInfoActivation::Enabled {
                        denied.push(record.provider.id.0.clone());
                    }
                }
                if !denied.is_empty() {
                    log::debug!("providers denied by policy: {denied:?}");
                }
                Ok(())
            }
        })
        .await;
    builtin.registrations.push(transform);
}

fn suppliers(entries: &[Entry]) -> HashMap<String, String> {
    let mut suppliers = HashMap::new();
    for entry in entries {
        let Entry::Document(document) = entry else { continue };
        let Origin::WellKnown { integration_id } = &document.origin else { continue };
        let Some(providers) = &document.info.providers else { continue };
        for (provider, _) in &providers.0 {
            suppliers.insert(provider.clone(), integration_id.clone());
        }
    }
    suppliers
}

fn read_accounts(credentials: &CredentialService) -> HashMap<String, Option<String>> {
    match credentials.all() {
        Ok(infos) => infos.into_iter().map(|info| (info.integration_id.0, account(&info.value))).collect(),
        Err(error) => {
            log::warn!("credential accounts were not read: {error}");
            HashMap::new()
        }
    }
}

fn account(value: &CredentialValue) -> Option<String> {
    let (key, metadata) = match value {
        CredentialValue::Key(key) => (Some(key.key.clone()), key.metadata.as_ref()),
        CredentialValue::Oauth(oauth) => (None, oauth.metadata.as_ref()),
        CredentialValue::External(external) => (None, external.metadata.as_ref()),
    };
    policy::account(&seams::CredentialValue {
        key,
        metadata: metadata.map(|metadata| metadata.0.iter().cloned().collect()).unwrap_or_default(),
    })
}

async fn subscribe_credentials(
    credentials: Arc<CredentialService>,
    provider: Arc<ProviderService>,
    mut subscription: opencode_bus::Subscription,
    accounts: Arc<RwLock<HashMap<String, Option<String>>>>,
) {
    while subscription.recv().await.is_some() {
        let next = read_accounts(&credentials);
        let changed = {
            let mut current = accounts.write().expect("the config policy account lock is never poisoned");
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
        provider.reload().await;
    }
}
