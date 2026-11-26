use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde_json::Value;

use crate::definition::{ToolOptions, ToolSpec};
use crate::registry::{ToolRegistration, ToolRegistry};
use crate::tools::{edit, patch, read, write};
use crate::{ToolContext, ToolError, ToolOutput, ToolRuntime};

#[derive(Clone, Default)]
pub struct BuiltinDeps {
    pub full_codemode: bool,
    pub read: Option<read::ReadDeps>,
    pub write: Option<write::WriteDeps>,
    pub edit: Option<edit::EditDeps>,
    pub patch: Option<patch::PatchDeps>,
}

pub async fn activate(registry: &ToolRegistry, deps: &BuiltinDeps) -> opencode_location::state::Registration {
    let entries = crate::generated::definitions(deps.full_codemode);
    let reader = deps.read.clone();
    let writer = deps.write.clone();
    let edit_deps = deps.edit.clone();
    let patch_deps = deps.patch.clone();
    registry
        .transform(move |editor| {
            for entry in entries {
                let definition = &entry.definition;
                let runtime: Arc<dyn ToolRuntime> =
                    match (definition.name.as_str(), &reader, &writer, &edit_deps, &patch_deps) {
                        ("read", Some(deps), _, _, _) => Arc::new(read::ReadRuntime::new(deps.clone())),
                        ("write", _, Some(deps), _, _) => Arc::new(write::WriteRuntime::new(deps.clone())),
                        ("edit", _, _, Some(deps), _) => Arc::new(edit::EditRuntime::new(deps.clone())),
                        ("patch", _, _, _, Some(deps)) => Arc::new(patch::PatchRuntime::new(deps.clone())),
                        _ => Arc::new(StubRuntime { name: definition.name.clone() }),
                    };
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
                    runtime,
                });
            }
        })
        .await
}

#[derive(Clone)]
struct StubRuntime {
    name: String,
}

impl ToolRuntime for StubRuntime {
    fn execute<'a>(&'a self, _input: Value, _context: &'a ToolContext) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        let name = self.name.clone();
        Box::pin(async move { Err(ToolError::new(format!("Tool {name} is not implemented"))) })
    }
}
