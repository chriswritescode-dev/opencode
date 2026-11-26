use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use opencode_config::wildcard;
use opencode_contract::generated::{PermissionEffect, PermissionRule};
use serde_json::Value;

use crate::context::{ToolContext, ToolError, ToolOutput};
use crate::definition::ToolDefinition;
use crate::image::Image;
use crate::registry::{RegisteredTool, ToolData};
use crate::runtime::{self, NoHooks, ToolHooks};

#[derive(Clone, Debug, Default)]
pub struct SnapshotOptions {
    pub hidden: BTreeSet<String>,
    pub direct: BTreeSet<String>,
}

#[derive(Clone, Debug)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
}

pub struct Snapshot {
    pub definitions: Vec<ToolDefinition>,
    pub entries: BTreeMap<String, RegisteredTool>,
    pub hooks: Arc<dyn ToolHooks>,
    pub image: Arc<Image>,
}

impl Snapshot {
    pub async fn execute(&self, call: ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        self.execute_with(call, context, None).await
    }

    pub async fn execute_with(
        &self,
        call: ToolCall,
        context: &ToolContext,
        definitions: Option<&BTreeMap<String, ToolDefinition>>,
    ) -> Result<ToolOutput, ToolError> {
        let requested = definitions.and_then(|definitions| definitions.get(&call.name));
        if requested.is_none() && definitions.is_some() && self.entries.contains_key(&call.name) {
            return Err(ToolError::new(format!("Tool is not available for this request: {}", call.name)));
        }
        let name = requested.map_or_else(|| call.name.clone(), |definition| definition.name.clone());
        let Some(tool) = self.entries.get(&name) else {
            return Err(ToolError::new(format!(
                "No tool named \"{name}\" is currently available. Please use a tool from the available tool list."
            )));
        };
        let context = ToolContext { call_id: call.id, ..context.clone() };
        runtime::run(tool, &name, call.input, &context, &self.entries, self.hooks.as_ref(), self.image.as_ref()).await
    }
}

pub fn snapshot(data: &ToolData, permissions: &[PermissionRule], options: &SnapshotOptions) -> Snapshot {
    let mut direct: BTreeMap<String, RegisteredTool> = BTreeMap::new();
    for (key, tool) in &data.tools {
        if options.hidden.contains(key) {
            continue;
        }
        let action = tool.options.permission.as_deref().unwrap_or(key);
        if wholly_disabled(action, permissions) {
            continue;
        }
        direct.insert(key.clone(), tool.clone());
    }
    let definitions = direct.keys().filter_map(|key| data.definitions.get(key).cloned()).collect();
    Snapshot { definitions, entries: direct, hooks: Arc::new(NoHooks), image: Arc::new(Image::new()) }
}

pub fn wholly_disabled(action: &str, rules: &[PermissionRule]) -> bool {
    rules
        .iter()
        .rev()
        .find(|rule| wildcard::matches(action, &rule.action, false))
        .is_some_and(|rule| rule.resource == "*" && rule.effect == PermissionEffect::Deny)
}
