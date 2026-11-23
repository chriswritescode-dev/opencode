use std::sync::Arc;

use serde_json::Value;

use crate::route::model::Model;
use crate::schema::{LlmRequest, Message, SystemMessage, ToolChanges, ToolDefinition, ToolEntry};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolUpdateKind {
    Add,
    Remove,
    Replace,
}

pub type PlacedGuard = Arc<dyn Fn(&LlmRequest, usize) -> bool + Send + Sync>;
pub type DeclarableGuard = Arc<dyn Fn(&ToolEntry) -> bool + Send + Sync>;

#[derive(Clone, Default)]
pub struct ToolUpdateEncoding {
    pub kinds: Vec<ToolUpdateKind>,
    pub placeable: Option<PlacedGuard>,
    pub declarable: Option<DeclarableGuard>,
}

pub fn tools_update(message: &Message) -> Option<&ToolChanges> {
    match message {
        Message::System(message) => message.tools.as_ref(),
        _ => None,
    }
}

pub fn adds_tools(request: &LlmRequest) -> bool {
    request.messages.iter().any(|message| tools_update(message).is_some_and(|changes| !changes.add.is_empty()))
}

pub fn tool_update_kinds(model: &Model) -> Vec<ToolUpdateKind> {
    let Some(encoding) = model.route.protocol.tool_updates() else {
        return Vec::new();
    };
    let Some(updates) = model
        .capabilities
        .as_ref()
        .and_then(|capabilities| capabilities.get("tools"))
        .and_then(|tools| tools.get("updates"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    updates
        .iter()
        .filter_map(|update| match update.as_str()? {
            "add" => Some(ToolUpdateKind::Add),
            "remove" => Some(ToolUpdateKind::Remove),
            "replace" => Some(ToolUpdateKind::Replace),
            _ => None,
        })
        .filter(|kind| encoding.kinds.contains(kind))
        .collect()
}

pub fn fold_tools(tools: Vec<ToolEntry>, add: &[ToolEntry], remove: &[String]) -> Vec<ToolEntry> {
    let mut added: Vec<(String, ToolEntry)> = Vec::new();
    for tool in add {
        let name = entry_name(tool).to_string();
        match added.iter_mut().find(|(existing, _)| existing == &name) {
            Some(entry) => entry.1 = tool.clone(),
            None => added.push((name, tool.clone())),
        }
    }
    let mut kept: Vec<ToolEntry> = Vec::new();
    for tool in tools {
        let name = entry_name(&tool).to_string();
        if remove.contains(&name) {
            continue;
        }
        let next = added.iter().find(|(added_name, _)| added_name == &name).map(|(_, tool)| tool.clone());
        kept.push(next.unwrap_or(tool));
    }
    let names = kept.iter().map(|tool| entry_name(tool).to_string()).collect::<Vec<_>>();
    let tail = added.into_iter().filter(|(name, _)| !names.contains(name)).map(|(_, tool)| tool).collect::<Vec<_>>();
    kept.into_iter().chain(tail).collect()
}

pub fn flatten_tools(tools: &[ToolEntry]) -> Vec<ToolDefinition> {
    let mut flat = Vec::new();
    flatten_into(tools, &[], &mut flat);
    let mut deduped: Vec<ToolDefinition> = Vec::new();
    for tool in flat {
        match deduped.iter_mut().find(|existing| existing.name == tool.name) {
            Some(existing) => *existing = tool,
            None => deduped.push(tool),
        }
    }
    deduped
}

fn flatten_into(tools: &[ToolEntry], path: &[String], flat: &mut Vec<ToolDefinition>) {
    for tool in tools {
        match tool {
            ToolEntry::Namespace(namespace) => {
                let mut nested = path.to_vec();
                nested.push(namespace.name.clone());
                flatten_into(&namespace.tools, &nested, flat);
            }
            ToolEntry::Tool(definition) => {
                let mut definition = definition.clone();
                if !path.is_empty() {
                    definition.name = format!("{}_{}", path.join("_"), definition.name);
                }
                flat.push(definition);
            }
        }
    }
}

pub fn withdrawn_tools(before: &[ToolEntry], after: &[ToolEntry]) -> Vec<String> {
    let kept = flatten_tools(after).into_iter().map(|tool| tool.name).collect::<Vec<_>>();
    flatten_tools(before).into_iter().map(|tool| tool.name).filter(|name| !kept.contains(name)).collect()
}

pub fn plan_tool_updates(model: &Model, request: LlmRequest) -> LlmRequest {
    let supported = tool_update_kinds(model);
    plan(model, request, &supported)
}

pub fn fold_tool_updates(model: &Model, request: LlmRequest) -> LlmRequest {
    plan(model, request, &[])
}

pub fn map_tool_updates(request: LlmRequest, f: &dyn Fn(Vec<ToolEntry>) -> Vec<ToolEntry>) -> LlmRequest {
    let mut request = request;
    if !request.messages.iter().any(|message| tools_update(message).is_some()) {
        return request;
    }
    request.messages = std::mem::take(&mut request.messages)
        .into_iter()
        .map(|message| match message {
            Message::System(mut system) => {
                if let Some(changes) = system.tools.take() {
                    system.tools = Some(ToolChanges { add: f(changes.add), remove: changes.remove });
                }
                Message::System(system)
            }
            message => message,
        })
        .collect();
    request
}

pub fn entry_name(tool: &ToolEntry) -> &str {
    match tool {
        ToolEntry::Tool(tool) => &tool.name,
        ToolEntry::Namespace(tool) => &tool.name,
    }
}

struct PlanStep {
    index: usize,
    native: bool,
    tools: Vec<ToolEntry>,
    changes: ToolChanges,
}

fn plan(model: &Model, request: LlmRequest, supported: &[ToolUpdateKind]) -> LlmRequest {
    let changes = request
        .messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| tools_update(message).map(|changes| (index, changes.clone())))
        .collect::<Vec<_>>();
    if changes.is_empty() {
        return request;
    }
    let mut request = request;
    let encoding = model.route.protocol.tool_updates();
    let declarable = encoding.as_ref().and_then(|encoding| encoding.declarable.as_ref());
    let placeable = encoding.as_ref().and_then(|encoding| encoding.placeable.as_ref());
    let first_turn = request.messages.iter().position(|message| !matches!(message, Message::System(_)));
    let mut tools = request.tools.clone();
    let mut declared = tools.iter().map(|tool| entry_name(tool).to_string()).collect::<Vec<_>>();
    let mut steps: Vec<PlanStep> = Vec::with_capacity(changes.len());

    for (index, change) in &changes {
        let added = change.add.iter().map(|tool| entry_name(tool).to_string()).collect::<Vec<_>>();
        let removed = tools
            .iter()
            .filter(|tool| {
                let name = entry_name(tool);
                change.remove.iter().any(|removed| removed == name) && !added.iter().any(|added| added == name)
            })
            .cloned()
            .collect::<Vec<_>>();
        let folded = fold_tools(tools.clone(), &change.add, &change.remove);
        let mut kinds = Vec::new();
        if !removed.is_empty() || !withdrawn_tools(&tools, &folded).is_empty() {
            kinds.push(ToolUpdateKind::Remove);
        }
        for name in &added {
            kinds.push(if declared.contains(name) { ToolUpdateKind::Replace } else { ToolUpdateKind::Add });
        }
        let native = first_turn.is_some_and(|first| *index > first)
            && kinds.iter().all(|kind| supported.contains(kind))
            && change.add.iter().chain(removed.iter()).all(|tool| declarable.is_none_or(|guard| guard(tool)))
            && placeable.is_none_or(|guard| guard(&request, *index));
        steps.push(PlanStep {
            index: *index,
            native,
            tools: folded.clone(),
            changes: ToolChanges {
                add: change.add.clone(),
                remove: removed.iter().map(|tool| entry_name(tool).to_string()).collect(),
            },
        });
        tools = folded;
        declared.extend(added);
    }

    let cut = steps.iter().rposition(|step| !step.native);
    let kept = steps
        .iter()
        .skip(cut.map_or(0, |cut| cut + 1))
        .map(|step| (step.index, step.changes.clone()))
        .collect::<Vec<_>>();
    let tools = match cut {
        Some(cut) => steps.get(cut).map(|step| step.tools.clone()).unwrap_or_default(),
        None => request.tools.clone(),
    };
    request.tools = tools;
    request.messages = std::mem::take(&mut request.messages)
        .into_iter()
        .enumerate()
        .filter_map(|(index, message)| match message {
            Message::System(system) if system.tools.is_some() => {
                let changes = kept.iter().find(|(kept_index, _)| *kept_index == index).map(|(_, changes)| changes);
                match changes {
                    Some(changes) if !changes.add.is_empty() || !changes.remove.is_empty() => {
                        Some(Message::System(SystemMessage { tools: Some(changes.clone()), ..system }))
                    }
                    _ if system.content.is_empty() => None,
                    _ => Some(Message::System(SystemMessage { tools: None, ..system })),
                }
            }
            message => Some(message),
        })
        .collect();
    request
}
