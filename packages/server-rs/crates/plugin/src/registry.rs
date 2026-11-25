use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use opencode_contract::codec::encode_json;
use opencode_location::location::LocationInfo;

use crate::generation::{ActivateContext, Activation, Features, Generation, Gui, Info, PluginState, Source};

#[derive(Debug, thiserror::Error)]
pub enum ActivateError {
    #[error("Duplicate plugin ID: {0}")]
    DuplicateId(String),
}

#[derive(Clone)]
pub struct Registry {
    inner: Arc<Inner>,
}

#[derive(Clone)]
pub struct WeakRegistry(std::sync::Weak<Inner>);

impl WeakRegistry {
    pub fn upgrade(&self) -> Option<Registry> {
        self.0.upgrade().map(|inner| Registry { inner })
    }
}

struct Inner {
    location: LocationInfo,
    publish: Arc<dyn Fn() + Send + Sync>,
    slots: Mutex<Vec<Slot>>,
    discovered: Mutex<Vec<Info>>,
    inventory: Mutex<Vec<Info>>,
    last_published: Mutex<Option<String>>,
    lock: tokio::sync::Mutex<()>,
    readiness: tokio::sync::watch::Sender<usize>,
    readiness_receiver: tokio::sync::watch::Receiver<usize>,
    closed: AtomicBool,
}

#[derive(Clone)]
struct Slot {
    plugin: Generation,
    activation: Option<Arc<ActivationCell>>,
    error: Option<String>,
    forgotten: bool,
}

pub struct ActivationCell {
    plugin: Generation,
    handle: tokio::sync::Mutex<Option<Box<dyn Activation>>>,
    failure: Mutex<Option<(String, String)>>,
}

impl ActivationCell {
    pub fn plugin(&self) -> &Generation {
        &self.plugin
    }

    pub fn failure(&self) -> Option<(String, String)> {
        self.failure.lock().expect("the plugin registry lock is never poisoned").clone()
    }
}

pub struct Hold {
    inner: Option<Arc<Inner>>,
}

impl Hold {
    pub fn release(mut self) {
        self.release_once();
    }

    fn release_once(&mut self) {
        if let Some(inner) = self.inner.take() {
            inner.readiness.send_modify(|pending| *pending = pending.saturating_sub(1));
        }
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        self.release_once();
    }
}

impl Registry {
    pub fn new(location: LocationInfo, publish: Arc<dyn Fn() + Send + Sync>) -> Registry {
        let (readiness, readiness_receiver) = tokio::sync::watch::channel(0);
        Registry {
            inner: Arc::new(Inner {
                location,
                publish,
                slots: Mutex::new(Vec::new()),
                discovered: Mutex::new(Vec::new()),
                inventory: Mutex::new(Vec::new()),
                last_published: Mutex::new(None),
                lock: tokio::sync::Mutex::new(()),
                readiness,
                readiness_receiver,
                closed: AtomicBool::new(false),
            }),
        }
    }

    pub async fn activate(&self, plugins: Vec<Generation>, failures: Vec<Info>) -> Result<(), ActivateError> {
        let mut ids = HashSet::new();
        for plugin in &plugins {
            if !ids.insert(plugin.id.as_str()) {
                return Err(ActivateError::DuplicateId(plugin.id.clone()));
            }
        }

        let hold = self.hold();
        let guard = self.inner.lock.lock().await;
        if self.inner.closed.load(Ordering::SeqCst) {
            return Ok(());
        }
        *self.inner.discovered.lock().expect("the plugin registry lock is never poisoned") = failures;

        let (prefix, unchanged) = {
            let mut slots = self.inner.slots.lock().expect("the plugin registry lock is never poisoned");
            let prefix = plugins
                .iter()
                .enumerate()
                .position(|(index, definition)| match slots.get(index) {
                    Some(slot) => {
                        slot.forgotten || slot.plugin.id != definition.id || slot.plugin.revision != definition.revision
                    }
                    None => true,
                })
                .unwrap_or(plugins.len());
            for (index, definition) in plugins.iter().take(prefix).enumerate() {
                if let Some(slot) = slots.get_mut(index) {
                    slot.plugin = definition.clone();
                }
            }
            (prefix, prefix == plugins.len() && slots.len() == plugins.len())
        };

        if unchanged {
            self.inner.refresh();
            return Ok(());
        }

        let previous: Vec<Slot> = {
            let mut slots = self.inner.slots.lock().expect("the plugin registry lock is never poisoned");
            slots.drain(prefix..).collect()
        };
        let by_id: HashMap<&str, &Slot> = previous.iter().map(|slot| (slot.plugin.id.as_str(), slot)).collect();

        for slot in previous.iter().rev() {
            let Some(activation) = &slot.activation else {
                continue;
            };
            if slot.forgotten {
                activation.handle.lock().await.take();
                continue;
            }
            if activation.failure().is_some() {
                continue;
            }
            let handle = activation.handle.lock().await.take();
            if let Some(handle) = handle
                && handle.close().await.is_err()
            {
                log::error!("failed to clean up plugin");
            }
        }

        for definition in &plugins[prefix..] {
            let previous_slot = by_id.get(definition.id.as_str()).copied();
            if let Some(slot) = previous_slot
                && let Some(activation) = &slot.activation
                && activation.failure().is_some()
                && slot.plugin.revision == definition.revision
            {
                self.push(Slot {
                    plugin: definition.clone(),
                    activation: Some(activation.clone()),
                    error: slot.error.clone(),
                    forgotten: false,
                });
                continue;
            }

            match self.load(definition).await {
                Ok(activation) => {
                    self.push(Slot {
                        plugin: definition.clone(),
                        activation: Some(activation),
                        error: None,
                        forgotten: false,
                    });
                }
                Err(error) => {
                    let restored =
                        match previous_slot.filter(|slot| !slot.forgotten).and_then(|slot| slot.activation.clone()) {
                            Some(fallback) if fallback.failure().is_none() => match self.load(&fallback.plugin).await {
                                Ok(activation) => Some(activation),
                                Err(_) => {
                                    log::error!("failed to restore plugin; deactivating");
                                    None
                                }
                            },
                            _ => None,
                        };
                    self.push(Slot {
                        plugin: definition.clone(),
                        activation: restored,
                        error: Some(error),
                        forgotten: false,
                    });
                }
            }
        }

        self.inner.refresh();
        drop(guard);
        drop(hold);
        Ok(())
    }

    pub fn list(&self) -> Vec<Info> {
        self.inner.inventory.lock().expect("the plugin registry lock is never poisoned").clone()
    }

    pub fn downgrade(&self) -> WeakRegistry {
        WeakRegistry(Arc::downgrade(&self.inner))
    }

    pub fn forget_script_slots(&self) {
        let mut slots = self.inner.slots.lock().expect("the plugin registry lock is never poisoned");
        for slot in slots.iter_mut() {
            if slot
                .plugin
                .source
                .as_ref()
                .is_some_and(|source| matches!(source, Source::Local { .. } | Source::Package { .. }))
            {
                slot.forgotten = true;
            }
        }
    }

    pub fn gui(&self, id: &str) -> Option<Gui> {
        self.inner
            .slots
            .lock()
            .expect("the plugin registry lock is never poisoned")
            .iter()
            .find(|slot| slot.plugin.id == id)
            .and_then(|slot| slot.plugin.gui.clone())
    }

    pub fn active_ids(&self) -> Vec<String> {
        self.inner
            .slots
            .lock()
            .expect("the plugin registry lock is never poisoned")
            .iter()
            .map(|slot| slot.plugin.id.clone())
            .collect()
    }

    pub fn hold(&self) -> Hold {
        if self.inner.closed.load(Ordering::SeqCst) {
            return Hold { inner: None };
        }
        self.inner.readiness.send_modify(|pending| *pending = pending.saturating_add(1));
        Hold { inner: Some(self.inner.clone()) }
    }

    pub async fn await_activation(&self) {
        let mut readiness = self.inner.readiness_receiver.clone();
        while *readiness.borrow() != 0 {
            if readiness.changed().await.is_err() {
                return;
            }
        }
    }

    pub fn disable(&self, activation: &Arc<ActivationCell>, state: &str) {
        *activation.failure.lock().expect("the plugin registry lock is never poisoned") = Some((
            format!("Plugin disabled after {state}.transform failed. Check server logs for details."),
            error_ref(),
        ));
        let hold = self.hold();
        let inner = self.inner.clone();
        let activation = activation.clone();
        tokio::spawn(async move {
            let guard = inner.lock.lock().await;
            if !inner.closed.load(Ordering::SeqCst) {
                inner.refresh();
            }
            drop(guard);
            let handle = activation.handle.lock().await.take();
            if let Some(handle) = handle
                && handle.close().await.is_err()
            {
                log::warn!("failed to clean up disabled plugin");
            }
            drop(hold);
        });
    }

    pub fn activation(&self, id: &str) -> Option<Arc<ActivationCell>> {
        self.inner
            .slots
            .lock()
            .expect("the plugin registry lock is never poisoned")
            .iter()
            .find(|slot| slot.plugin.id == id)
            .and_then(|slot| slot.activation.clone())
    }

    pub async fn close(&self) {
        let _guard = self.inner.lock.lock().await;
        self.inner.closed.store(true, Ordering::SeqCst);
        self.inner.readiness.send_modify(|pending| *pending = 0);
        let slots = {
            let mut slots = self.inner.slots.lock().expect("the plugin registry lock is never poisoned");
            std::mem::take(&mut *slots)
        };
        for slot in slots.into_iter().rev() {
            let Some(activation) = slot.activation else {
                continue;
            };
            if slot.forgotten {
                activation.handle.lock().await.take();
                continue;
            }
            if activation.failure().is_some() {
                continue;
            }
            let handle = activation.handle.lock().await.take();
            if let Some(handle) = handle
                && handle.close().await.is_err()
            {
                log::error!("failed to clean up plugin");
            }
        }
    }

    async fn load(&self, definition: &Generation) -> Result<Arc<ActivationCell>, String> {
        let context = ActivateContext { location: self.inner.location.clone(), options: definition.options.clone() };
        match definition.activator.activate(context).await {
            Ok(handle) => Ok(Arc::new(ActivationCell {
                plugin: definition.clone(),
                handle: tokio::sync::Mutex::new(Some(handle)),
                failure: Mutex::new(None),
            })),
            Err(error) => {
                log::warn!("failed to load plugin");
                Err(error)
            }
        }
    }

    fn push(&self, slot: Slot) {
        self.inner.slots.lock().expect("the plugin registry lock is never poisoned").push(slot);
    }
}

impl Inner {
    fn refresh(&self) {
        let inventory = self.snapshot();
        let gui: Vec<Option<String>> = self
            .slots
            .lock()
            .expect("the plugin registry lock is never poisoned")
            .iter()
            .map(|slot| slot.plugin.gui.as_ref().map(|gui| gui.revision.clone()))
            .collect();
        let key =
            format!("{}{}", encode_json(&inventory.iter().map(Info::to_wire).collect::<Vec<_>>()), encode_json(&gui));
        *self.inventory.lock().expect("the plugin registry lock is never poisoned") = inventory;
        let changed = {
            let mut last = self.last_published.lock().expect("the plugin registry lock is never poisoned");
            let changed = last.as_deref() != Some(key.as_str());
            if changed {
                *last = Some(key);
            }
            changed
        };
        if changed {
            (self.publish)();
        }
    }

    fn snapshot(&self) -> Vec<Info> {
        let slots = self.slots.lock().expect("the plugin registry lock is never poisoned");
        let mut inventory: Vec<Info> = slots.iter().map(slot_info).collect();
        inventory.extend(self.discovered.lock().expect("the plugin registry lock is never poisoned").iter().cloned());
        inventory
    }
}

fn slot_info(slot: &Slot) -> Info {
    let failure = slot
        .activation
        .as_ref()
        .and_then(|activation| activation.failure())
        .map(|(error, reference)| (error, Some(reference)))
        .or_else(|| slot.error.clone().map(|error| (error, None)));
    Info {
        id: Some(slot.plugin.id.clone()),
        source: slot.plugin.source.clone().unwrap_or(Source::Builtin),
        features: slot.plugin.features.clone().unwrap_or_else(Features::server_only),
        state: match failure {
            Some((error, reference)) => PluginState::Failed { error, reference },
            None => PluginState::Active,
        },
    }
}

pub(crate) fn error_ref() -> String {
    let bytes: [u8; 4] = rand::random();
    format!("err_{}", bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>())
}
