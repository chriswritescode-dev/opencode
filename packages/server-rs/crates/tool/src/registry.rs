use std::sync::Arc;

use futures_util::future::BoxFuture;
use indexmap::IndexMap;
use opencode_contract::generated::PermissionRule;
use opencode_location::state::{Options, Registration, State, StateError};

use crate::ToolRuntime;
use crate::definition::{self, ToolDefinition, ToolOptions, ToolSpec};
use crate::snapshot::{Snapshot, SnapshotOptions};

#[derive(Clone)]
pub struct ToolRegistration {
    pub spec: ToolSpec,
    pub options: ToolOptions,
    pub runtime: Arc<dyn ToolRuntime>,
}

#[derive(Clone)]
pub struct RegisteredTool {
    pub key: String,
    pub name: String,
    pub namespace: Option<String>,
    pub definition: ToolDefinition,
    pub options: ToolOptions,
    pub runtime: Arc<dyn ToolRuntime>,
}

impl RegisteredTool {
    pub fn registration(&self) -> ToolRegistration {
        ToolRegistration {
            spec: ToolSpec {
                name: self.name.clone(),
                description: Some(self.definition.description.clone()),
                input_schema: Some(self.definition.input_schema.clone()),
                output_schema: self.definition.output_schema.clone(),
                freeform: self.definition.freeform.clone(),
                cache: self.definition.cache.clone(),
                native: self.definition.native.clone(),
            },
            options: self.options.clone(),
            runtime: self.runtime.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolNamespace {
    pub name: String,
    pub description: Option<String>,
}

#[derive(Clone, Debug)]
pub struct RegistrationFailure {
    pub kind: &'static str,
    pub name: String,
    pub namespace: Option<String>,
    pub message: String,
}

#[derive(Default)]
pub struct ToolData {
    pub tools: IndexMap<String, RegisteredTool>,
    pub definitions: IndexMap<String, ToolDefinition>,
    pub namespaces: IndexMap<String, ToolNamespace>,
    pub errors: Vec<RegistrationFailure>,
}

pub struct ToolEditor<'a> {
    data: &'a mut ToolData,
}

impl<'a> ToolEditor<'a> {
    pub fn new(data: &'a mut ToolData) -> Self {
        ToolEditor { data }
    }

    pub fn list(&self) -> Vec<RegisteredTool> {
        self.data.tools.values().cloned().collect()
    }

    pub fn get(&self, key: &str) -> Option<RegisteredTool> {
        self.data.tools.get(key).cloned()
    }

    pub fn namespace(&mut self, namespace: ToolNamespace) {
        if let Some(error) = definition::namespace_error(&namespace.name) {
            self.data.errors.push(RegistrationFailure {
                kind: "namespace",
                name: namespace.name.clone(),
                namespace: Some(namespace.name),
                message: error.message,
            });
            return;
        }
        self.data.namespaces.insert(namespace.name.clone(), namespace);
    }

    pub fn add(&mut self, registration: ToolRegistration) {
        self.commit(registration);
    }

    pub fn update<F>(&mut self, key: &str, update: F)
    where
        F: FnOnce(&mut ToolRegistration),
    {
        let Some(current) = self.data.tools.get(key) else { return };
        let mut draft = current.registration();
        update(&mut draft);
        draft.spec.name = current.name.clone();
        draft.options.namespace = current.namespace.clone();
        self.commit(draft);
    }

    pub fn remove(&mut self, key: &str) {
        self.data.tools.shift_remove(key);
        self.data.definitions.shift_remove(key);
    }

    fn commit(&mut self, mut registration: ToolRegistration) {
        if registration.spec.input_schema.is_none() {
            registration.spec.input_schema = Some(serde_json::json!({}));
        }
        match definition::register(&registration.spec, &registration.options) {
            Ok((key, tool_definition)) => {
                let tool = RegisteredTool {
                    key: key.clone(),
                    name: registration.spec.name.clone(),
                    namespace: registration.options.namespace.clone(),
                    definition: tool_definition.clone(),
                    options: registration.options,
                    runtime: registration.runtime,
                };
                self.data.tools.insert(key.clone(), tool);
                self.data.definitions.insert(key, tool_definition);
            }
            Err(error) => self.data.errors.push(RegistrationFailure {
                kind: "tool",
                name: registration.spec.name.clone(),
                namespace: registration.options.namespace.clone(),
                message: error.message,
            }),
        }
    }
}

#[derive(Clone)]
pub struct ToolRegistry {
    state: State<ToolData>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        ToolRegistry {
            state: State::new(Options {
                name: Some("tool".to_string()),
                notify: Some(Box::new(notify)),
                ..Options::new(ToolData::default)
            }),
        }
    }

    pub async fn transform<F>(&self, f: F) -> Registration
    where
        F: Fn(&mut ToolEditor) + Send + Sync + 'static,
    {
        self.state
            .transform(move |data| {
                let mut editor = ToolEditor::new(data);
                f(&mut editor);
                Ok(())
            })
            .await
    }

    pub async fn list(&self) -> Result<Vec<RegisteredTool>, StateError> {
        Ok(self.state.get().await?.tools.values().cloned().collect())
    }

    pub async fn get(&self, key: &str) -> Result<Option<RegisteredTool>, StateError> {
        Ok(self.state.get().await?.tools.get(key).cloned())
    }

    pub async fn namespaces(&self) -> Result<Vec<ToolNamespace>, StateError> {
        Ok(self.state.get().await?.namespaces.values().cloned().collect())
    }

    pub async fn errors(&self) -> Result<Vec<RegistrationFailure>, StateError> {
        Ok(self.state.get().await?.errors.clone())
    }

    pub async fn snapshot(
        &self,
        permissions: &[PermissionRule],
        options: &SnapshotOptions,
    ) -> Result<Snapshot, StateError> {
        let data = self.state.get().await?;
        Ok(crate::snapshot::snapshot(&data, permissions, options))
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn notify(data: Arc<ToolData>) -> BoxFuture<'static, ()> {
    for failure in &data.errors {
        match failure.kind {
            "namespace" => {
                log::error!("Skipping invalid namespace registration: {} ({})", failure.name, failure.message)
            }
            _ => log::error!(
                "Skipping invalid tool registration: {} namespace={:?} ({})",
                failure.name,
                failure.namespace,
                failure.message
            ),
        }
    }
    Box::pin(async {})
}
