use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

use crate::generation::{Activator, Generation, Source};

struct Inner {
    plugins: Mutex<Vec<Generation>>,
    revision: AtomicU64,
    updates: broadcast::Sender<()>,
}

#[derive(Clone)]
pub struct SdkPlugins {
    inner: Arc<Inner>,
}

impl Default for SdkPlugins {
    fn default() -> Self {
        let (updates, _) = broadcast::channel(16);
        Self { inner: Arc::new(Inner { plugins: Mutex::new(Vec::new()), revision: AtomicU64::new(0), updates }) }
    }
}

impl SdkPlugins {
    pub fn register(&self, id: impl Into<String>, activator: Arc<dyn Activator>) {
        let id = id.into();
        let revision = self.inner.revision.fetch_add(1, Ordering::Relaxed) + 1;
        let generation = Generation {
            id: id.clone(),
            revision: revision.to_string(),
            source: Some(Source::Sdk),
            features: None,
            gui: None,
            options: serde_json::Map::new(),
            activator,
        };
        {
            let mut plugins = self.inner.plugins.lock().expect("the sdk plugin list is never poisoned");
            match plugins.iter_mut().find(|existing| existing.id == id) {
                Some(existing) => *existing = generation,
                None => plugins.push(generation),
            }
        }
        let _ = self.inner.updates.send(());
    }

    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.inner.updates.subscribe()
    }

    pub fn all(&self) -> Vec<Generation> {
        self.inner.plugins.lock().expect("the sdk plugin list is never poisoned").clone()
    }
}

pub struct InstancePlugins(Vec<Generation>);

impl InstancePlugins {
    pub fn empty() -> Self {
        Self(Vec::new())
    }

    pub fn all(&self) -> Vec<Generation> {
        self.0.clone()
    }

    pub fn bound(plugins: Vec<(String, Arc<dyn Activator>)>) -> Result<Self, String> {
        let mut seen = HashSet::new();
        let mut duplicates = Vec::new();
        for (id, _) in &plugins {
            if !seen.insert(id) {
                duplicates.push(id.clone());
            }
        }
        if !duplicates.is_empty() {
            return Err(format!("duplicate instance plugin ids: {}", duplicates.join(", ")));
        }
        Ok(Self(
            plugins
                .into_iter()
                .map(|(id, activator)| Generation {
                    id,
                    revision: "instance".to_string(),
                    source: Some(Source::Sdk),
                    features: None,
                    gui: None,
                    options: serde_json::Map::new(),
                    activator,
                })
                .collect(),
        ))
    }
}
