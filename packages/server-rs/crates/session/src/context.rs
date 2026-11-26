use std::fmt;

use opencode_ai::Model;
use opencode_catalog::model::ModelService;
use opencode_catalog::resolver::{self, Resolver};
use opencode_contract::generated::{AgentInfo, ModelInfo, ModelRef, SessionInfo, SessionMessageInfo};
use opencode_registry::agent::AgentService;
use opencode_storage::Database;

use crate::instruction_state;
use crate::instructions::{self, BuiltinsInput, List, Rendered};
use crate::store::{MessagesInput, Order, SessionStore};

pub struct Loaded {
    pub session: SessionInfo,
    pub agent: AgentInfo,
    pub model: ModelInfo,
    pub variant: Option<String>,
    pub runtime: Model,
    pub initial: Vec<Rendered>,
    pub messages: Vec<SessionMessageInfo>,
}

impl Loaded {
    pub fn model_ref(&self) -> ModelRef {
        ModelRef {
            id: self.model.id.clone(),
            provider_id: self.model.provider_id.clone(),
            variant: self.variant.clone(),
        }
    }
}

pub struct LoadInput<'a> {
    pub database: &'a Database,
    pub store: &'a SessionStore,
    pub models: &'a ModelService,
    pub resolver: &'a Resolver,
    pub agents: &'a AgentService,
    pub builtins: BuiltinsInput,
}

#[derive(Debug)]
pub enum ContextError {
    SessionNotFound(String),
    AgentNotFound { session_id: String, agent_id: String },
    Agents(String),
    ModelNotSelected { session_id: String },
    ModelUnavailable { provider_id: String, model_id: String },
    ModelResolution(resolver::Error),
    Instructions(String),
    History(String),
    Database(String),
}

impl fmt::Display for ContextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SessionNotFound(session_id) => write!(formatter, "Session not found: {session_id}"),
            Self::AgentNotFound { agent_id, .. } => write!(formatter, "Agent not found: \"{agent_id}\""),
            Self::Agents(message) => write!(formatter, "Agents unavailable: {message}"),
            Self::ModelNotSelected { session_id } => {
                write!(formatter, "No model is available for session {session_id}")
            }
            Self::ModelUnavailable { provider_id, model_id } => {
                write!(formatter, "Model unavailable: {provider_id}/{model_id}")
            }
            Self::ModelResolution(error) => write!(formatter, "{error}"),
            Self::Instructions(message) => write!(formatter, "Instructions unavailable: {message}"),
            Self::History(message) => write!(formatter, "Session history unavailable: {message}"),
            Self::Database(message) => write!(formatter, "Session store unavailable: {message}"),
        }
    }
}

impl std::error::Error for ContextError {}

pub async fn load(input: &LoadInput<'_>, session_id: &str) -> Result<Loaded, ContextError> {
    let list = instructions::builtins(input.builtins.clone())
        .map_err(|error| ContextError::Instructions(error.to_string()))?;
    load_with(input, session_id, list).await
}

pub async fn load_with(input: &LoadInput<'_>, session_id: &str, list: List) -> Result<Loaded, ContextError> {
    let session = require_session(input.store, session_id)?;
    let agent = require_agent(input.agents, &session).await?;
    let (model, variant) = select_model(input, &session, &agent, session_id).await?;
    let runtime =
        input.resolver.resolve_model(&model, variant.as_deref()).await.map_err(ContextError::ModelResolution)?.model;
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
    Ok(Loaded { session, agent, model, variant, runtime, initial, messages })
}

fn require_session(store: &SessionStore, session_id: &str) -> Result<SessionInfo, ContextError> {
    store
        .get(session_id)
        .map_err(|error| ContextError::Database(error.to_string()))?
        .ok_or_else(|| ContextError::SessionNotFound(session_id.to_owned()))
}

async fn require_agent(agents: &AgentService, session: &SessionInfo) -> Result<AgentInfo, ContextError> {
    let selection = agents
        .select(session.agent.as_ref().map(|id| id.0.as_str()))
        .await
        .map_err(|error| ContextError::Agents(error.to_string()))?;
    let Some(info) = selection.info else {
        let agent_id = session.agent.as_ref().map_or(selection.id, |id| id.0.clone());
        return Err(ContextError::AgentNotFound { session_id: session.id.0.clone(), agent_id });
    };
    Ok(info)
}

async fn select_model(
    input: &LoadInput<'_>,
    session: &SessionInfo,
    agent: &AgentInfo,
    session_id: &str,
) -> Result<(ModelInfo, Option<String>), ContextError> {
    if let Some(reference) = &session.model {
        let model = find_model(input.models, reference).await?.ok_or_else(|| ContextError::ModelUnavailable {
            provider_id: reference.provider_id.0.clone(),
            model_id: reference.id.clone(),
        })?;
        return Ok((model, reference.variant.clone()));
    }
    if let Some(reference) = &agent.model
        && let Some(model) = find_model(input.models, reference).await?
    {
        return Ok((model, reference.variant.clone()));
    }
    let fallback = input.resolver.fallback().await.map_err(ContextError::ModelResolution)?;
    fallback
        .map(|model| (model, None))
        .ok_or_else(|| ContextError::ModelNotSelected { session_id: session_id.to_owned() })
}

async fn find_model(models: &ModelService, reference: &ModelRef) -> Result<Option<ModelInfo>, ContextError> {
    let available = models.available(None).await.map_err(|error| ContextError::ModelResolution(error.into()))?;
    Ok(available.into_iter().find(|model| model.provider_id == reference.provider_id && model.id == reference.id))
}
