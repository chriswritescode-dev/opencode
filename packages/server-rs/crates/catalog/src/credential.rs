use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use opencode_bus::{Bus, BusError, Definition, PublishOptions};
use opencode_config::seams::{self, CredentialLookup};
use opencode_contract::codec::{self, Decode};
use opencode_contract::generated::{CredentialID, CredentialOAuth, CredentialValue, IntegrationID};
use opencode_storage::credential::{self, CredentialRow, NewCredential};
use opencode_storage::{Database, StorageError, id};
use rusqlite::TransactionBehavior;
use serde_json::Value;
use tokio::sync::watch;

use crate::lock::with_lock;

pub const REFRESH_MARGIN_MS: i64 = 5 * 60 * 1000;

const REFRESH_TIMEOUT: Duration = Duration::from_secs(30);

pub type RefreshError = Arc<dyn std::error::Error + Send + Sync>;

type Outcome = Result<Option<CredentialOAuth>, RefreshError>;
type Progress = watch::Receiver<Option<Outcome>>;
type Start = Option<(u64, watch::Sender<Option<Outcome>>)>;

struct Shared {
    generation: u64,
    sender: watch::Sender<Option<Outcome>>,
}

#[derive(Debug, thiserror::Error)]
#[error("credential refresh timed out")]
struct RefreshTimedOut;

#[derive(Debug, thiserror::Error)]
#[error("credential refresh task ended without a result")]
struct RefreshAborted;

#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    pub id: CredentialID,
    pub integration_id: IntegrationID,
    pub label: String,
    pub value: CredentialValue,
}

#[derive(Clone, Debug)]
pub struct CreateInput {
    pub id: Option<CredentialID>,
    pub integration_id: IntegrationID,
    pub value: CredentialValue,
    pub label: Option<String>,
    pub activate: Option<bool>,
}

#[derive(Clone, Debug, Default)]
pub struct Update {
    pub label: Option<String>,
    pub value: Option<CredentialValue>,
}

#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Bus(#[from] BusError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Decode(#[from] codec::DecodeError),
}

pub struct CredentialService {
    database: Arc<Database>,
    bus: Arc<Bus>,
    locks: PathBuf,
    pending: Mutex<HashMap<String, Shared>>,
    generation: AtomicU64,
}

struct Removal {
    integration_id: Option<IntegrationID>,
    switched: bool,
    credential_id: Option<String>,
}

pub fn expiring(value: &CredentialOAuth, now: i64) -> bool {
    expiring_with_margin(value, now, REFRESH_MARGIN_MS)
}

pub fn expiring_with_margin(value: &CredentialOAuth, now: i64, margin_ms: i64) -> bool {
    value.expires != 0.0 && value.expires <= (now + margin_ms) as f64
}

impl CredentialService {
    pub fn new(database: Arc<Database>, bus: Arc<Bus>, locks: impl Into<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            database,
            bus,
            locks: locks.into(),
            pending: Mutex::new(HashMap::new()),
            generation: AtomicU64::new(0),
        })
    }

    pub fn all(&self) -> Result<Vec<Info>, CredentialError> {
        infos(self.database.read(credential::list_all)?)
    }

    pub fn list(&self, integration_id: &IntegrationID) -> Result<Vec<Info>, CredentialError> {
        infos(self.database.read(|db| credential::list(db, &integration_id.0))?)
    }

    pub fn get(&self, id: &CredentialID) -> Result<Option<Info>, CredentialError> {
        match self.database.read(|db| credential::get(db, &id.0))? {
            Some(row) => info(row),
            None => Ok(None),
        }
    }

    pub fn create(&self, input: CreateInput) -> Result<Info, CredentialError> {
        let requested_activate = input.activate;
        let info = Info {
            id: input.id.unwrap_or_else(|| CredentialID(format!("cred_{}", id::ascending()))),
            integration_id: input.integration_id,
            label: input.label.unwrap_or_else(|| "default".to_owned()),
            value: input.value,
        };
        let encoded = codec::encode_json(&info.value);
        let activate = self.database.transaction(TransactionBehavior::Immediate, |tx| {
            let current = credential::list(tx, &info.integration_id.0)?.pop();
            let activate = requested_activate != Some(false) || current.is_none();
            if !activate
                && let Some(current) = &current
                && current.active != Some(true)
            {
                credential::pin_active(tx, &current.id)?;
            }
            credential::insert(
                tx,
                &NewCredential {
                    id: &info.id.0,
                    integration_id: &info.integration_id.0,
                    label: &info.label,
                    value: &encoded,
                    active: activate,
                },
            )?;
            if activate {
                credential::set_active(tx, &info.integration_id.0, &info.id.0)?;
            }
            Ok::<bool, StorageError>(activate)
        })?;
        self.publish_updated(&info.integration_id, Some(&info.id.0))?;
        if activate {
            self.publish_switched(&info.integration_id, Some(&info.id.0))?;
        }
        Ok(info)
    }

    pub fn activate(&self, id: &CredentialID) -> Result<(), CredentialError> {
        let integration_id = self.database.transaction(TransactionBehavior::Immediate, |tx| {
            let Some(row) = credential::get(tx, &id.0)? else {
                return Ok(None);
            };
            let Some(integration_id) = row.integration_id else {
                return Ok(None);
            };
            let current = credential::list(tx, &integration_id)?.pop();
            if current.as_ref().map(|current| current.id.as_str()) == Some(id.0.as_str()) {
                return Ok(None);
            }
            credential::set_active(tx, &integration_id, &id.0)?;
            Ok::<Option<IntegrationID>, StorageError>(Some(IntegrationID(integration_id)))
        })?;
        if let Some(integration_id) = &integration_id {
            self.publish_switched(integration_id, Some(&id.0))?;
        }
        Ok(())
    }

    pub fn update(&self, id: &CredentialID, update: Update) -> Result<(), CredentialError> {
        if update.label.is_none() && update.value.is_none() {
            return Ok(());
        }
        let Some(row) = self.database.read(|db| credential::get(db, &id.0))? else {
            return Ok(());
        };
        let Some(integration_id) = row.integration_id else {
            return Ok(());
        };
        let label_changed = update.label.as_deref().is_some_and(|label| label != row.label);
        if update.label.as_deref() == Some(row.label.as_str()) && update.value.is_none() {
            return Ok(());
        }
        let encoded = update.value.as_ref().map(codec::encode_json);
        let changed = self.database.transaction(TransactionBehavior::Immediate, |tx| {
            let mut changed = true;
            if let Some(label) = &update.label {
                changed = credential::update_label(tx, &id.0, label)? && changed;
            }
            if let Some(value) = &encoded {
                changed = credential::update_value(tx, &id.0, value)? && changed;
            }
            Ok::<bool, StorageError>(changed)
        })?;
        if !changed {
            return Ok(());
        }
        if label_changed {
            self.publish_updated(&IntegrationID(integration_id), Some(&id.0))?;
        }
        Ok(())
    }

    pub fn remove(&self, id: &CredentialID) -> Result<(), CredentialError> {
        let removal = self.database.transaction(TransactionBehavior::Immediate, |tx| {
            let Some(row) = credential::get(tx, &id.0)? else {
                return Ok(None);
            };
            let current = match row.integration_id.as_deref() {
                Some(integration_id) => credential::list(tx, integration_id)?.pop(),
                None => None,
            };
            credential::delete(tx, &id.0)?;
            let Some(integration_id) = row.integration_id else {
                return Ok(Some(Removal { integration_id: None, switched: false, credential_id: None }));
            };
            if current.as_ref().map(|current| current.id.as_str()) != Some(id.0.as_str()) {
                return Ok(Some(Removal {
                    integration_id: Some(IntegrationID(integration_id)),
                    switched: false,
                    credential_id: None,
                }));
            }
            let replacement = credential::list(tx, &integration_id)?
                .into_iter()
                .max_by(|a, b| a.time_created.cmp(&b.time_created).then_with(|| a.id.cmp(&b.id)));
            if let Some(replacement) = &replacement {
                credential::set_active(tx, &integration_id, &replacement.id)?;
            }
            Ok::<Option<Removal>, StorageError>(Some(Removal {
                integration_id: Some(IntegrationID(integration_id)),
                switched: true,
                credential_id: replacement.map(|replacement| replacement.id),
            }))
        })?;
        let Some(removal) = removal else {
            return Ok(());
        };
        if let Some(integration_id) = &removal.integration_id {
            self.publish_updated(integration_id, Some(&id.0))?;
        }
        if removal.switched
            && let Some(integration_id) = &removal.integration_id
        {
            self.publish_switched(integration_id, removal.credential_id.as_deref())?;
        }
        Ok(())
    }

    pub async fn refresh<F, Fut>(
        self: &Arc<Self>,
        id: &CredentialID,
        stale: impl Fn(&CredentialOAuth, i64) -> bool + Send + Sync + 'static,
        refresh: F,
    ) -> Result<Option<CredentialOAuth>, RefreshError>
    where
        F: FnOnce(CredentialOAuth) -> Fut + Send + 'static,
        Fut: Future<Output = Result<CredentialOAuth, RefreshError>> + Send,
    {
        let (start, progress) = self.begin(id);
        if let Some((generation, sender)) = start {
            let service = self.clone();
            let key = id.0.clone();
            tokio::spawn(async move {
                let outcome = service.refresh_locked(&key, stale, refresh).await;
                {
                    let mut pending = service.pending.lock().expect("lock the pending refreshes");
                    if pending.get(&key).is_some_and(|shared| shared.generation == generation) {
                        pending.remove(&key);
                    }
                }
                sender.send_replace(Some(outcome));
            });
        }
        settle(progress).await
    }

    fn begin(&self, id: &CredentialID) -> (Start, Progress) {
        let mut pending = self.pending.lock().expect("lock the pending refreshes");
        if let Some(shared) = pending.get(&id.0) {
            return (None, shared.sender.subscribe());
        }
        let generation = self.generation.fetch_add(1, Ordering::Relaxed);
        let (sender, progress) = watch::channel(None);
        pending.insert(id.0.clone(), Shared { generation, sender: sender.clone() });
        (Some((generation, sender)), progress)
    }

    async fn refresh_locked<F, Fut>(
        self: &Arc<Self>,
        id: &str,
        stale: impl Fn(&CredentialOAuth, i64) -> bool + Send + Sync,
        refresh: F,
    ) -> Outcome
    where
        F: FnOnce(CredentialOAuth) -> Fut + Send,
        Fut: Future<Output = Result<CredentialOAuth, RefreshError>> + Send,
    {
        let locks = self.locks.clone();
        let key = format!("credential-refresh:{id}");
        let service = self.clone();
        with_lock(&locks, &key, opencode_local::lock::Options::default(), async move {
            let credential_id = CredentialID(id.to_owned());
            let Some(current) = service.get(&credential_id).map_err(shared)? else {
                return Ok(None);
            };
            let CredentialValue::Oauth(current) = current.value else {
                return Ok(None);
            };
            if !stale(&current, opencode_util::time::now_ms()) {
                log::info!("credential refresh skipped credentialID={id} reason=already_refreshed");
                return Ok(Some(current));
            }
            let value = match tokio::time::timeout(REFRESH_TIMEOUT, refresh(current.clone())).await {
                Ok(value) => value?,
                Err(_) => return Err(Arc::new(RefreshTimedOut) as RefreshError),
            };
            let Some(latest) = service.get(&credential_id).map_err(shared)? else {
                return Ok(None);
            };
            let CredentialValue::Oauth(latest) = latest.value else {
                return Ok(None);
            };
            if latest.access != current.access || latest.refresh != current.refresh {
                log::warn!("credential refresh discarded credentialID={id} reason=changed_meanwhile");
                return Ok(Some(latest));
            }
            service
                .update(&credential_id, Update { label: None, value: Some(CredentialValue::Oauth(value.clone())) })
                .map_err(shared)?;
            Ok(Some(value))
        })
        .await
        .map_err(|error| Arc::new(error) as RefreshError)?
    }

    fn publish_updated(
        &self,
        integration_id: &IntegrationID,
        credential_id: Option<&str>,
    ) -> Result<(), CredentialError> {
        self.publish("credential.updated", integration_id, credential_id)
    }

    fn publish_switched(
        &self,
        integration_id: &IntegrationID,
        credential_id: Option<&str>,
    ) -> Result<(), CredentialError> {
        self.publish("credential.switched", integration_id, credential_id)
    }

    fn publish(
        &self,
        r#type: &'static str,
        integration_id: &IntegrationID,
        credential_id: Option<&str>,
    ) -> Result<(), CredentialError> {
        let definition = Definition::ephemeral(r#type);
        self.bus.publish(
            &definition,
            serde_json::json!({ "integrationID": &integration_id.0, "credentialID": credential_id }),
            PublishOptions { global: true, ..Default::default() },
        )?;
        Ok(())
    }
}

async fn settle(mut progress: Progress) -> Outcome {
    loop {
        if let Some(outcome) = (*progress.borrow_and_update()).clone() {
            return outcome;
        }
        if progress.changed().await.is_err() {
            return Err(Arc::new(RefreshAborted));
        }
    }
}

fn shared(error: CredentialError) -> RefreshError {
    Arc::new(error)
}

impl CredentialLookup for CredentialService {
    fn latest(&self, integration_id: &str) -> Option<seams::CredentialValue> {
        self.list(&IntegrationID(integration_id.to_owned())).ok()?.pop().map(|info| seam_value(&info.value))
    }

    fn get(&self, credential_id: &str) -> Option<seams::CredentialValue> {
        CredentialService::get(self, &CredentialID(credential_id.to_owned())).ok()?.map(|info| seam_value(&info.value))
    }
}

fn infos(rows: Vec<CredentialRow>) -> Result<Vec<Info>, CredentialError> {
    rows.into_iter().filter_map(|row| info(row).transpose()).collect()
}

fn info(row: CredentialRow) -> Result<Option<Info>, CredentialError> {
    let Some(integration_id) = row.integration_id else {
        return Ok(None);
    };
    Ok(Some(Info {
        id: CredentialID(row.id),
        integration_id: IntegrationID(integration_id),
        label: row.label,
        value: decode_value(&row.value)?,
    }))
}

fn decode_value(text: &str) -> Result<CredentialValue, CredentialError> {
    let mut value: Value = serde_json::from_str(text)?;
    if let Some(object) = value.as_object_mut()
        && object.get("type").and_then(Value::as_str) == Some("key")
        && let Some(configuration) = object.remove("configuration")
    {
        let Value::Object(configuration) = configuration else {
            return Err(codec::DecodeError::new(&codec::Path::new(), "expected a form answer object").into());
        };
        match object.get_mut("metadata") {
            Some(Value::Object(metadata)) => {
                for (key, value) in configuration {
                    metadata.insert(key, value);
                }
            }
            None => {
                object.insert("metadata".to_owned(), Value::Object(configuration));
            }
            Some(_) => {}
        }
    }
    Ok(CredentialValue::decode(&value, &mut codec::Path::new())?)
}

fn seam_value(value: &CredentialValue) -> seams::CredentialValue {
    let (key, metadata) = match value {
        CredentialValue::Key(key) => (Some(key.key.clone()), key.metadata.as_ref()),
        CredentialValue::Oauth(oauth) => (None, oauth.metadata.as_ref()),
        CredentialValue::External(external) => (None, external.metadata.as_ref()),
    };
    seams::CredentialValue {
        key,
        metadata: metadata.map(|metadata| metadata.0.iter().cloned().collect()).unwrap_or_default(),
    }
}
