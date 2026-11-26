use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde_json::Value;

use crate::definition::{ToolOptions, ToolSpec};
use crate::registry::{ToolRegistration, ToolRegistry};
use crate::{ToolContext, ToolError, ToolOutput, ToolRuntime};

#[derive(Clone, Debug, Default)]
pub struct BuiltinDeps {
    pub full_codemode: bool,
}

pub async fn activate(registry: &ToolRegistry, deps: &BuiltinDeps) -> opencode_location::state::Registration {
    let entries = crate::generated::definitions(deps.full_codemode);
    registry
        .transform(move |editor| {
            for entry in entries {
                let definition = &entry.definition;
                editor.add(ToolRegistration {
                    spec: ToolSpec {
                        name: definition.name.clone(),
                        description: Some(definition.description.clone()),
                        input_schema: Some(definition.input_schema.clone()),
                        output_schema: definition.output_schema.clone(),
                        freeform: definition.freeform.clone(),
                        cache: definition.cache.clone(),
                        native: definition.native.clone(),
                    },
                    options: ToolOptions {
                        namespace: None,
                        permission: entry.permission.clone(),
                        codemode: entry.codemode.clone(),
                    },
                    runtime: Arc::new(StubRuntime { name: definition.name.clone() }),
                });
            }
        })
        .await
}

#[derive(Clone, Debug)]
struct StubRuntime {
    name: String,
}

impl ToolRuntime for StubRuntime {
    fn execute<'a>(&'a self, _input: Value, _context: &'a ToolContext) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        let name = self.name.clone();
        Box::pin(async move { Err(ToolError::new(format!("Tool {name} is not implemented"))) })
    }
}
