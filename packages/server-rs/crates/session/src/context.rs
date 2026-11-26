use std::fmt;
use std::sync::Arc;

use opencode_ai::{Env, Model, RuntimeModel};
use opencode_catalog::Catalog;
use opencode_contract::generated::{AgentInfo, ModelInfo, ModelRef, SessionInfo, SessionMessageInfo};
use opencode_registry::agent::AgentService;
use opencode_storage::Database;
use opencode_tool::{LocationTools, SnapshotOptions};

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
    pub snapshot: Arc<opencode_tool::Snapshot>,
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
    pub agents: &'a AgentService,
    pub env: &'a dyn Env,
    pub builtins: BuiltinsInput,
    pub tools: &'a LocationTools,
}

#[derive(Debug)]
pub enum ContextError {
    SessionNotFound(String),
    AgentNotFound { session_id: String, agent_id: String },
    Agents(String),
    ModelNotSelected { session_id: String },
    ModelUnavailable { provider_id: String, model_id: String },
    ModelResolution(String),
    Instructions(String),
    History(String),
    Database(String),
    Tools(String),
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
            Self::ModelResolution(message) => write!(formatter, "{message}"),
            Self::Instructions(message) => write!(formatter, "Instructions unavailable: {message}"),
            Self::History(message) => write!(formatter, "Session history unavailable: {message}"),
            Self::Database(message) => write!(formatter, "Session store unavailable: {message}"),
            Self::Tools(message) => write!(formatter, "Tools unavailable: {message}"),
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
    let snapshot = Arc::new(
        snapshot(input.tools, &agent, &session, &model)
            .await
            .map_err(|error| ContextError::Tools(error.to_string()))?,
    );
    Ok(Loaded { session, agent, model, runtime, initial, messages, snapshot })
}

async fn snapshot(
    tools: &LocationTools,
    agent: &AgentInfo,
    session: &SessionInfo,
    model: &ModelInfo,
) -> Result<opencode_tool::Snapshot, opencode_location::state::StateError> {
    let permissions = opencode_permission::rule::merge(&[
        agent.permissions.as_slice(),
        session.permissions.as_deref().unwrap_or(&[]),
    ]);
    let policy = opencode_tool::policy::model_tools(
        Some(&opencode_tool::policy::ModelInfo { id: model.id.clone(), model_id: model.model_id.clone() }),
        false,
        tools.full_codemode() && !opencode_tool::snapshot::wholly_disabled("execute", &permissions),
    );
    tools.snapshot(&permissions, &SnapshotOptions { hidden: policy.hidden, direct: policy.direct }).await
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
        Ok(_) => Err(model_unavailable(model)),
        Err(error) => Err(ContextError::ModelResolution(error.to_string())),
    }
}

fn model_unavailable(model: &ModelInfo) -> ContextError {
    ContextError::ModelUnavailable { provider_id: model.provider_id.0.clone(), model_id: model.id.clone() }
}
