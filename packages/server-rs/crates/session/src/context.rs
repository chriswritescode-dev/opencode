use std::fmt;

use opencode_ai::{Env, Model, RuntimeModel};
use opencode_catalog::Catalog;
use opencode_contract::generated::{AgentInfo, ModelInfo, ModelRef, SessionInfo, SessionMessageInfo};
use opencode_storage::Database;

use crate::agent::Agents;
use crate::agent_instructions;
use crate::instruction_state;
use crate::instructions::{self, BuiltinsInput, List, Rendered};
use crate::store::{MessagesInput, Order, SessionStore};

pub struct Loaded {
    pub session: SessionInfo,
    pub agent: AgentInfo,
    pub model: ModelInfo,
    pub runtime: Model,
    pub initial: Vec<Rendered>,
    pub messages: Vec<SessionMessageInfo>,
}

impl Loaded {
    pub fn model_ref(&self) -> ModelRef {
        ModelRef { id: self.model.id.clone(), provider_id: self.model.provider_id.clone(), variant: None }
    }
}

pub struct LoadInput<'a> {
    pub database: &'a Database,
    pub store: &'a SessionStore,
    pub catalog: &'a Catalog,
    pub agents: &'a Agents,
    pub env: &'a dyn Env,
    pub builtins: BuiltinsInput,
}

#[derive(Debug)]
pub enum ContextError {
    SessionNotFound(String),
    AgentNotFound { session_id: String, agent_id: String },
    ModelNotSelected { session_id: String },
    ModelUnavailable { provider_id: String, model_id: String },
    Instructions(String),
    History(String),
    Database(String),
}

impl fmt::Display for ContextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SessionNotFound(session_id) => write!(formatter, "Session not found: {session_id}"),
            Self::AgentNotFound { agent_id, .. } => write!(formatter, "Agent not found: \"{agent_id}\""),
            Self::ModelNotSelected { session_id } => {
                write!(formatter, "No model is available for session {session_id}")
            }
            Self::ModelUnavailable { provider_id, model_id } => {
                write!(formatter, "Model unavailable: {provider_id}/{model_id}")
            }
            Self::Instructions(message) => write!(formatter, "Instructions unavailable: {message}"),
            Self::History(message) => write!(formatter, "Session history unavailable: {message}"),
            Self::Database(message) => write!(formatter, "Session store unavailable: {message}"),
        }
    }
}

impl std::error::Error for ContextError {}

pub fn instruction_list(
    store: &SessionStore,
    agents: &Agents,
    builtins: BuiltinsInput,
    session_id: &str,
) -> Result<List, ContextError> {
    let session = require_session(store, session_id)?;
    let agent = require_agent(agents, &session)?;
    let mut list = instructions::builtins(builtins).map_err(|error| ContextError::Instructions(error.to_string()))?;
    list.push(agent_instructions::source(&agent));
    Ok(list)
}

pub fn load(input: &LoadInput<'_>, session_id: &str) -> Result<Loaded, ContextError> {
    let list = instruction_list(input.store, input.agents, input.builtins.clone(), session_id)?;
    load_with(input, session_id, list)
}

pub fn load_with(input: &LoadInput<'_>, session_id: &str, list: List) -> Result<Loaded, ContextError> {
    let session = require_session(input.store, session_id)?;
    let agent = require_agent(input.agents, &session)?;
    let model = require_model(input.catalog, &session, &agent, session_id)?;
    let runtime = resolve_runtime(input.catalog, input.env, &model)?;
    let initial = instruction_state::initial(input.database, session_id, &list)
        .map_err(|error| ContextError::Instructions(error.to_string()))?;
    let messages = input
        .store
        .messages(&MessagesInput {
            session_id: session_id.to_owned(),
            limit: None,
            order: Some(Order::Asc),
            r#type: None,
            cursor: None,
        })
        .map_err(|error| ContextError::History(error.to_string()))?;
    Ok(Loaded { session, agent, model, runtime, initial, messages })
}

fn require_session(store: &SessionStore, session_id: &str) -> Result<SessionInfo, ContextError> {
    store
        .get(session_id)
        .map_err(|error| ContextError::Database(error.to_string()))?
        .ok_or_else(|| ContextError::SessionNotFound(session_id.to_owned()))
}

fn require_agent(agents: &Agents, session: &SessionInfo) -> Result<AgentInfo, ContextError> {
    let selection = agents.select(session.agent.as_ref().map(|id| id.0.as_str()));
    let Some(info) = selection.info else {
        let agent_id = session.agent.as_ref().map_or(selection.id, |id| id.0.clone());
        return Err(ContextError::AgentNotFound { session_id: session.id.0.clone(), agent_id });
    };
    Ok(info)
}

fn require_model(
    catalog: &Catalog,
    session: &SessionInfo,
    agent: &AgentInfo,
    session_id: &str,
) -> Result<ModelInfo, ContextError> {
    let models = catalog.models(None);
    if let Some(reference) = &session.model {
        return find_model(&models, reference).ok_or_else(|| ContextError::ModelUnavailable {
            provider_id: reference.provider_id.0.clone(),
            model_id: reference.id.clone(),
        });
    }
    if let Some(reference) = &agent.model
        && let Some(model) = find_model(&models, reference)
    {
        return Ok(model);
    }
    catalog.default_model().ok_or_else(|| ContextError::ModelNotSelected { session_id: session_id.to_owned() })
}

fn find_model(models: &[ModelInfo], reference: &ModelRef) -> Option<ModelInfo> {
    models.iter().find(|model| model.provider_id.0 == reference.provider_id.0 && model.id == reference.id).cloned()
}

fn resolve_runtime(catalog: &Catalog, env: &dyn Env, model: &ModelInfo) -> Result<Model, ContextError> {
    let reference = ModelRef { id: model.id.clone(), provider_id: model.provider_id.clone(), variant: None };
    match catalog.runtime_model(&reference, env) {
        Ok(RuntimeModel::Language(runtime)) => Ok(runtime),
        _ => Err(model_unavailable(model)),
    }
}

fn model_unavailable(model: &ModelInfo) -> ContextError {
    ContextError::ModelUnavailable { provider_id: model.provider_id.0.clone(), model_id: model.id.clone() }
}
