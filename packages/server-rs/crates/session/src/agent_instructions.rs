use opencode_contract::generated::{AgentInfo, AgentInfoInstructions};
use serde_json::{Value, json};

use crate::instructions::{Observed, Source};

const KEY: &str = "core/agent";

pub fn source(agent: &AgentInfo) -> Source {
    let agent = agent.clone();
    Source {
        key: KEY.to_string(),
        read: Box::new(move || selected(&agent).map_or(Observed::Removed, Observed::Value)),
        initial: Box::new(|value| decode(value).map(|selected| selected.active)),
        changed: Box::new(|previous, current| {
            let current = decode(current)?;
            let previous = match decode(previous) {
                Some(previous) => previous,
                None => return Some(current.active),
            };
            if previous.id != current.id {
                return Some(format!("{}\n\n{}", previous.inactive, current.active));
            }
            if previous.active == current.active {
                return None;
            }
            Some(current.changed)
        }),
        removed: Some(Box::new(|previous| decode(previous).map(|previous| previous.inactive))),
    }
}

struct Selected {
    id: String,
    active: String,
    inactive: String,
    changed: String,
}

fn selected(agent: &AgentInfo) -> Option<Value> {
    let selected = match agent.instructions.as_ref()? {
        AgentInfoInstructions::Object(value) => Selected {
            id: agent.id.0.clone(),
            active: value.selected.clone(),
            inactive: value.unselected.clone(),
            changed: value.selected.clone(),
        },
        AgentInfoInstructions::String(text) => Selected {
            id: agent.id.0.clone(),
            active: format!(
                "You are the {} agent. Follow these instructions while you are the {} agent:\n\n{text}",
                agent.name, agent.name
            ),
            changed: format!(
                "Your instructions as the {} agent changed. These replace the previous ones:\n\n{text}",
                agent.name
            ),
            inactive: format!(
                "You are no longer the {} agent. Stop following the instructions you were given as the {} agent. Keep following anything the user asked for.",
                agent.name, agent.name
            ),
        },
    };
    Some(json!({
        "id": selected.id,
        "active": selected.active,
        "inactive": selected.inactive,
        "changed": selected.changed,
    }))
}

fn decode(value: &Value) -> Option<Selected> {
    let object = value.as_object()?;
    Some(Selected {
        id: object.get("id")?.as_str()?.to_string(),
        active: object.get("active")?.as_str()?.to_string(),
        inactive: object.get("inactive")?.as_str()?.to_string(),
        changed: object.get("changed")?.as_str()?.to_string(),
    })
}
