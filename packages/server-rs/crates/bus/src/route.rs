use std::collections::HashMap;
use std::sync::Arc;

use opencode_contract::generated::{AbsolutePath, DURABLE_EVENTS, LocationRef, WorkspaceID};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

use crate::event::{Event, parse_location};

#[derive(Default)]
pub(crate) struct Routes {
    sessions: HashMap<String, LocationRef>,
}

pub(crate) struct PendingRoutes {
    updates: HashMap<String, Option<LocationRef>>,
    resolved: Vec<Option<Arc<[LocationRef]>>>,
}

impl Routes {
    pub fn prepare(&self, connection: &Connection, events: &[&Event]) -> rusqlite::Result<PendingRoutes> {
        let mut updates = HashMap::new();
        let mut resolved = Vec::with_capacity(events.len());
        for event in events {
            let Some(session_id) = session_id(event) else {
                resolved.push(None);
                continue;
            };
            if event.r#type() == "session.created" {
                let declared = data_location(event);
                updates.insert(session_id, declared.clone());
                resolved.push(Some(Arc::from(event.location().or(declared).into_iter().collect::<Vec<_>>())));
                continue;
            }
            if event.location().is_some() && !matches!(event.r#type(), "session.forked" | "session.moved") {
                if event.r#type() == "session.deleted" {
                    updates.insert(session_id, None);
                }
                resolved.push(None);
                continue;
            }
            let owner = if event.r#type() == "session.forked" {
                event.data().get("parentID").and_then(Value::as_str).map_or_else(|| session_id.clone(), str::to_string)
            } else {
                session_id.clone()
            };
            let mut reference = match updates.get(&owner) {
                Some(reference) => reference.clone(),
                None => self.sessions.get(&owner).cloned(),
            };
            if reference.is_none() && !updates.contains_key(&owner) {
                reference = session_location(connection, &owner)?;
                updates.insert(owner, reference.clone());
            }
            if event.r#type() == "session.moved" {
                let destination = data_location(event);
                updates.insert(session_id, destination.clone());
                resolved.push(Some(Arc::from(moved_routes(reference, destination))));
                continue;
            }
            if event.r#type() == "session.forked" {
                updates.insert(session_id.clone(), reference.clone());
            }
            resolved.push(Some(Arc::from(delivered_routes(event, reference))));
            if event.r#type() == "session.deleted" {
                updates.insert(session_id, None);
            }
        }
        Ok(PendingRoutes { updates, resolved })
    }

    pub fn remove(&mut self, session_id: &str) {
        self.sessions.remove(session_id);
    }
}

impl PendingRoutes {
    pub fn apply(&self, routes: &mut Routes) {
        for (session_id, reference) in &self.updates {
            match reference {
                Some(reference) => {
                    routes.sessions.insert(session_id.clone(), reference.clone());
                }
                None => {
                    routes.sessions.remove(session_id);
                }
            }
        }
    }

    pub fn routes(&self, index: usize) -> Option<Arc<[LocationRef]>> {
        self.resolved.get(index).cloned().flatten()
    }
}

fn session_id(event: &Event) -> Option<String> {
    if !is_session_event(event.r#type()) {
        return None;
    }
    event.data().get("sessionID").and_then(Value::as_str).map(str::to_string)
}

const EPHEMERAL_SESSION_EVENT_TYPES: [&str; 6] = [
    "session.usage.updated",
    "session.text.delta",
    "session.reasoning.delta",
    "session.tool.input.delta",
    "session.tool.progress",
    "session.compaction.delta",
];

fn is_session_event(r#type: &str) -> bool {
    EPHEMERAL_SESSION_EVENT_TYPES.contains(&r#type)
        || DURABLE_EVENTS.iter().any(|(name, _, aggregate)| *name == r#type && *aggregate == "sessionID")
}

fn data_location(event: &Event) -> Option<LocationRef> {
    event.data().get("location").and_then(parse_location)
}

fn moved_routes(reference: Option<LocationRef>, destination: Option<LocationRef>) -> Vec<LocationRef> {
    match (reference, destination) {
        (Some(reference), Some(destination)) => vec![reference, destination],
        (Some(reference), None) => vec![reference],
        (None, Some(destination)) => vec![destination],
        (None, None) => Vec::new(),
    }
}

fn delivered_routes(event: &Event, reference: Option<LocationRef>) -> Vec<LocationRef> {
    match event.location() {
        Some(location) => vec![location],
        None => reference.into_iter().collect(),
    }
}

fn session_location(connection: &Connection, session_id: &str) -> rusqlite::Result<Option<LocationRef>> {
    let row = connection
        .prepare_cached("SELECT directory, workspace_id FROM session_v2 WHERE id = ?1")?
        .query_row([session_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)))
        .optional()?;
    Ok(row.map(|(directory, workspace_id)| LocationRef {
        directory: AbsolutePath(directory),
        workspace_id: workspace_id.map(WorkspaceID),
    }))
}
