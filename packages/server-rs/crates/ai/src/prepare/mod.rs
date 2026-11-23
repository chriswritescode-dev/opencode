pub mod cache_policy;
pub mod effort_updates;
pub mod gemini_json_schema;
pub mod tool_history;
pub mod tool_schema;
pub mod tool_updates;

use crate::route::auth::set_header;
use crate::route::model::Model;
use crate::schema::{
    LlmRequest, StringMap, ToolEntry, merge_generation, merge_http, merge_provider_options, merge_string_maps,
    merge_timeout,
};

pub use cache_policy::{apply_cache_policy, places_markers, request_ttl};
pub use effort_updates::{apply_effort_updates, effort_update, resolve_effort_updates, strip_effort_updates};
pub use tool_history::normalize_tool_history;
pub use tool_schema::model_compatibility;
pub use tool_updates::{
    adds_tools, flatten_tools, fold_tool_updates, fold_tools, map_tool_updates, plan_tool_updates, tool_update_kinds,
    tools_update, withdrawn_tools,
};

pub fn resolve_request_options(model: &Model, request: LlmRequest) -> LlmRequest {
    let mut resolved = request;
    let messages = std::mem::take(&mut resolved.messages);
    resolved.messages = normalize_tool_history(messages);
    if let Some(options) = &model.options {
        resolved.generation = merge_generation(&[Some(&options.generation), Some(&resolved.generation)]);
        resolved.provider_options =
            merge_provider_options(&[options.provider_options.as_ref(), resolved.provider_options.as_ref()]);
        resolved.headers = merge_string_maps(&[options.headers.as_ref(), resolved.headers.as_ref()]);
        resolved.timeout = merge_timeout(&[options.timeout.as_ref(), resolved.timeout.as_ref()]);
        resolved.http = merge_http(&[options.http.as_ref(), resolved.http.as_ref()]);
    }
    resolved
}

pub fn prepare(model: &Model, request: LlmRequest) -> LlmRequest {
    let original = resolve_request_options(model, request);
    let project = |tools: Vec<ToolEntry>| tool_schema::tools(dedupe_tools(tools), model);
    let mut updated = plan_tool_updates(model, original);
    updated.tools = project(std::mem::take(&mut updated.tools));
    let projected = map_tool_updates(updated, &project);
    let effort = apply_effort_updates(model, projected);
    let mut resolved = apply_cache_policy(model, effort);
    merge_route_headers(model, &mut resolved);
    resolved
}

fn dedupe_tools(tools: Vec<ToolEntry>) -> Vec<ToolEntry> {
    let mut deduped: Vec<ToolEntry> = Vec::new();
    for tool in tools {
        let key = entry_key(&tool);
        match deduped.iter_mut().find(|existing| entry_key(existing) == key) {
            Some(existing) => *existing = tool,
            None => deduped.push(tool),
        }
    }
    deduped
        .into_iter()
        .map(|tool| match tool {
            ToolEntry::Namespace(mut namespace) => {
                namespace.tools = dedupe_tools(namespace.tools);
                ToolEntry::Namespace(namespace)
            }
            tool => tool,
        })
        .collect()
}

fn entry_key(tool: &ToolEntry) -> String {
    match tool {
        ToolEntry::Tool(tool) => format!("tool:{}", tool.name),
        ToolEntry::Namespace(tool) => format!("namespace:{}", tool.name),
    }
}

fn merge_route_headers(model: &Model, request: &mut LlmRequest) {
    let mut merged: Vec<(String, String)> = model.route.protocol.required_headers(request);
    if let Some(factory) = &model.route.headers {
        for (name, value) in factory(request) {
            merged = set_header(merged, name, value);
        }
    }
    if let Some(headers) = &request.headers {
        for (name, value) in headers.iter() {
            merged = set_header(merged, name, value);
        }
    }
    if merged.is_empty() {
        return;
    }
    request.headers = Some(StringMap::from_pairs(merged));
}
