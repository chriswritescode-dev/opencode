use std::sync::Arc;

use indexmap::IndexMap;
use opencode_contract::codec::{Encode, JsonMap};
use opencode_contract::generated::{
    AgentID, AgentInfo, AgentInfoMode, PermissionEffect, PermissionRule, ProviderRequest, ProviderSettings,
};
use opencode_location::state::{Options, Registration, State, StateError, changes};

use crate::events::RegistryEvents;

pub const DEFAULT_ID: &str = "build";

pub fn default_info(id: &str) -> AgentInfo {
    AgentInfo {
        id: AgentID(id.to_string()),
        name: id.to_string(),
        model: None,
        request: ProviderRequest {
            settings: ProviderSettings {
                timeout: None,
                header_timeout: None,
                chunk_timeout: None,
                compaction: None,
                cache: None,
                transport: None,
                encoding: None,
                tls: None,
                rest: JsonMap(Vec::new()),
            },
            headers: JsonMap(Vec::new()),
            body: JsonMap(Vec::new()),
        },
        system: None,
        description: None,
        mode: AgentInfoMode::Primary,
        hidden: false,
        color: None,
        steps: None,
        permissions: vec![
            rule("*", "*", PermissionEffect::Allow),
            rule("read", "*.env", PermissionEffect::Ask),
            rule("read", "*.env.*", PermissionEffect::Ask),
            rule("read", "*.env.example", PermissionEffect::Allow),
        ],
    }
}

#[derive(Default)]
pub struct AgentData {
    pub agents: IndexMap<String, AgentInfo>,
    pub default: Option<String>,
}

pub struct AgentEditor<'a> {
    data: &'a mut AgentData,
}

impl<'a> AgentEditor<'a> {
    pub fn new(data: &'a mut AgentData) -> Self {
        AgentEditor { data }
    }

    pub fn list(&self) -> Vec<&AgentInfo> {
        self.data.agents.values().collect()
    }

    pub fn get(&self, id: &str) -> Option<&AgentInfo> {
        self.data.agents.get(id)
    }

    pub fn default(&mut self, id: Option<&str>) {
        self.data.default = id.map(str::to_string);
    }

    pub fn update<F: FnOnce(&mut AgentInfo)>(&mut self, id: &str, f: F) {
        let info = self.data.agents.entry(id.to_string()).or_insert_with(|| default_info(id));
        f(info);
        info.id = AgentID(id.to_string());
    }

    pub fn remove(&mut self, id: &str) {
        self.data.agents.shift_remove(id);
    }
}

pub struct Selection {
    pub id: String,
    pub info: Option<AgentInfo>,
}

#[derive(Clone)]
pub struct AgentService {
    state: State<AgentData>,
}

impl AgentService {
    pub fn new(events: Arc<dyn RegistryEvents>) -> Self {
        AgentService {
            state: State::new(Options {
                name: Some("agent".to_string()),
                notify: Some(changes(
                    |data: &AgentData| projection(data),
                    move |_| {
                        events.updated("agent.updated");
                        Box::pin(async {})
                    },
                )),
                ..Options::new(AgentData::default)
            }),
        }
    }

    pub async fn get(&self, id: &str) -> Result<Option<AgentInfo>, StateError> {
        Ok(self.state.get().await?.agents.get(id).cloned())
    }

    pub async fn resolve(&self, id: Option<&str>) -> Result<Option<AgentInfo>, StateError> {
        let data = self.state.get().await?;
        match id {
            Some(id) => Ok(data.agents.get(id).cloned()),
            None => Ok(selected_default(&data).cloned()),
        }
    }

    pub async fn select(&self, id: Option<&str>) -> Result<Selection, StateError> {
        let data = self.state.get().await?;
        let Some(id) = id else {
            let info = selected_default(&data).cloned();
            let id = info.as_ref().map_or_else(|| DEFAULT_ID.to_string(), |info| info.id.0.clone());
            return Ok(Selection { id, info });
        };
        Ok(Selection { id: id.to_string(), info: data.agents.get(id).cloned() })
    }

    pub async fn list(&self) -> Result<Vec<AgentInfo>, StateError> {
        let data = self.state.get().await?;
        Ok(ordered(&data).into_iter().cloned().collect())
    }

    pub async fn transform<F>(&self, f: F) -> Registration
    where
        F: for<'a, 'b> Fn(&'a mut AgentEditor<'b>) + Send + Sync + 'static,
    {
        self.state
            .transform(move |data| {
                let mut editor = AgentEditor::new(data);
                f(&mut editor);
                Ok(())
            })
            .await
    }

    pub async fn reload(&self) {
        self.state.reload().await;
    }
}

fn rule(action: &str, resource: &str, effect: PermissionEffect) -> PermissionRule {
    PermissionRule { action: action.to_string(), resource: resource.to_string(), effect }
}

fn is_selectable(info: &AgentInfo) -> bool {
    info.mode != AgentInfoMode::Subagent && !info.hidden
}

fn selectable(info: Option<&AgentInfo>) -> Option<&AgentInfo> {
    info.filter(|info| is_selectable(info))
}

fn selected_default(data: &AgentData) -> Option<&AgentInfo> {
    data.default
        .as_deref()
        .and_then(|id| selectable(data.agents.get(id)))
        .or_else(|| selectable(data.agents.get(DEFAULT_ID)))
        .or_else(|| data.agents.values().find(|info| is_selectable(info)))
}

fn ordered(data: &AgentData) -> Vec<&AgentInfo> {
    let agents: Vec<&AgentInfo> = data.agents.values().collect();
    let Some(selected) = selected_default(data) else {
        return agents;
    };
    let mut result = vec![selected];
    result.extend(agents.into_iter().filter(|info| info.id != selected.id));
    result
}

fn projection(data: &AgentData) -> String {
    let mut out = String::new();
    for info in ordered(data) {
        info.encode(&mut out);
    }
    out
}
