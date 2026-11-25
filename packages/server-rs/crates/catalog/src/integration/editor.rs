use std::collections::HashMap;
use std::sync::Arc;

use opencode_contract::generated::{IntegrationID, IntegrationInfo, IntegrationMethod};

use crate::integration::{IntegrationEntry, IntegrationRegistry, OAuthImplementation};

pub enum Implementation {
    OAuth(Arc<dyn OAuthImplementation>),
    Method { integration_id: IntegrationID, method: IntegrationMethod },
}

pub struct Editor<'a> {
    pub(crate) registry: &'a mut IntegrationRegistry,
}

impl Editor<'_> {
    pub fn list(&self) -> Vec<IntegrationInfo> {
        self.registry.entries.values().map(|entry| entry.info.clone()).collect()
    }

    pub fn get(&self, id: &IntegrationID) -> Option<IntegrationInfo> {
        self.registry.entries.get(&id.0).map(|entry| entry.info.clone())
    }

    pub fn update(&mut self, id: &IntegrationID, update: impl FnOnce(&mut IntegrationInfo)) {
        let entry = self.ensure(id);
        update(&mut entry.info);
        entry.info.id = id.clone();
    }

    pub fn remove(&mut self, id: &IntegrationID) {
        self.registry.entries.shift_remove(&id.0);
    }

    pub fn method_list(&self, integration_id: &IntegrationID) -> Vec<IntegrationMethod> {
        self.registry.entries.get(&integration_id.0).map(|entry| entry.info.methods.clone()).unwrap_or_default()
    }

    pub fn method_update(&mut self, implementation: Implementation) {
        let (integration_id, method) = match implementation {
            Implementation::OAuth(implementation) => {
                let integration_id = implementation.integration_id().clone();
                let method = implementation.method().clone();
                let entry = self.ensure(&integration_id);
                entry.implementations.insert(method.id.0.clone(), implementation);
                (integration_id, IntegrationMethod::Oauth(method))
            }
            Implementation::Method { integration_id, method } => (integration_id, method),
        };
        let entry = self.ensure(&integration_id);
        match entry.info.methods.iter_mut().find(|existing| same_method(existing, &method)) {
            Some(existing) => *existing = method,
            None => entry.info.methods.push(method),
        }
    }

    pub fn method_remove(&mut self, integration_id: &IntegrationID, method: &IntegrationMethod) {
        let Some(entry) = self.registry.entries.get_mut(&integration_id.0) else {
            return;
        };
        entry.info.methods.retain(|candidate| !same_method(candidate, method));
        if let IntegrationMethod::Oauth(method) = method {
            entry.implementations.remove(&method.id.0);
        }
    }

    fn ensure(&mut self, id: &IntegrationID) -> &mut IntegrationEntry {
        self.registry.entries.entry(id.0.clone()).or_insert_with(|| IntegrationEntry {
            info: IntegrationInfo {
                id: id.clone(),
                name: id.0.clone(),
                metadata: None,
                methods: Vec::new(),
                connections: Vec::new(),
            },
            implementations: HashMap::new(),
        })
    }
}

fn same_method(a: &IntegrationMethod, b: &IntegrationMethod) -> bool {
    match (a, b) {
        (IntegrationMethod::Oauth(a), IntegrationMethod::Oauth(b)) => a.id == b.id,
        (IntegrationMethod::Command(a), IntegrationMethod::Command(b)) => a.id == b.id,
        (IntegrationMethod::External(a), IntegrationMethod::External(b)) => a.id == b.id,
        (IntegrationMethod::Key(_), IntegrationMethod::Key(_)) => true,
        (IntegrationMethod::Env(_), IntegrationMethod::Env(_)) => true,
        _ => false,
    }
}
