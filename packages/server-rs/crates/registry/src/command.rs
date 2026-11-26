use std::sync::Arc;

use indexmap::IndexMap;
use opencode_contract::codec::Encode;
use opencode_contract::generated::requests::{
    PromptAgentAttachment, PromptInputFileAttachment, PromptInputSkillAttachment,
};
use opencode_contract::generated::{AgentInfo, CommandInfo, ModelRef, SessionID, SessionInboxDelivery};
use opencode_location::state::{Options, Registration, State, StateError, changes};

use crate::events::RegistryEvents;

pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Prompt {
    pub text: String,
    pub files: Option<Vec<PromptInputFileAttachment>>,
    pub agents: Option<Vec<PromptAgentAttachment>>,
    pub skills: Option<Vec<PromptInputSkillAttachment>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Invocation {
    pub session_id: SessionID,
    pub prompt: Prompt,
    pub delivery: SessionInboxDelivery,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PromptRequest {
    pub session_id: SessionID,
    pub prompt: Prompt,
    pub delivery: SessionInboxDelivery,
    pub resume: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SubagentRequest {
    pub parent_session_id: SessionID,
    pub title: String,
    pub description: String,
    pub agent: Option<String>,
    pub model: Option<ModelRef>,
    pub prompt: Prompt,
}

pub trait CommandRuntime: Send + Sync {
    fn prompt<'a>(&'a self, request: PromptRequest) -> BoxFuture<'a, Result<(), String>>;

    fn switch_agent_model<'a>(
        &'a self,
        session_id: SessionID,
        agent: Option<String>,
        model: Option<ModelRef>,
    ) -> BoxFuture<'a, Result<(), String>>;

    fn start_subagent<'a>(&'a self, request: SubagentRequest) -> BoxFuture<'a, Result<(), String>>;

    fn run_shell<'a>(&'a self, command: String, cwd: std::path::PathBuf) -> BoxFuture<'a, Result<String, String>>;

    fn resolve_agent<'a>(&'a self, id: String) -> BoxFuture<'a, Result<AgentInfo, String>>;
}

pub trait CommandExecute: Send + Sync {
    fn execute<'a>(
        &'a self,
        invocation: Invocation,
        runtime: &'a dyn CommandRuntime,
    ) -> BoxFuture<'a, Result<(), String>>;
}

#[derive(Clone)]
pub struct CommandDefinition {
    pub name: String,
    pub description: Option<String>,
    pub execute: Arc<dyn CommandExecute>,
}

#[derive(Default)]
pub struct CommandData {
    pub commands: IndexMap<String, CommandDefinition>,
}

pub struct CommandEditor<'a> {
    data: &'a mut CommandData,
}

impl<'a> CommandEditor<'a> {
    pub fn new(data: &'a mut CommandData) -> Self {
        CommandEditor { data }
    }

    pub fn add(&mut self, definition: CommandDefinition) {
        self.data.commands.insert(definition.name.clone(), definition);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandError {
    NotFound { command: String, message: String },
    Execution { command: String, message: String },
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommandError::NotFound { message, .. } => f.write_str(message),
            CommandError::Execution { message, .. } => f.write_str(message),
        }
    }
}

impl std::error::Error for CommandError {}

#[derive(Clone)]
pub struct CommandService {
    state: State<CommandData>,
}

impl CommandService {
    pub fn new(events: Arc<dyn RegistryEvents>) -> Self {
        CommandService {
            state: State::new(Options {
                name: Some("command".to_string()),
                notify: Some(changes(
                    |data: &CommandData| projection(data),
                    move |_| {
                        events.updated("command.updated");
                        Box::pin(async {})
                    },
                )),
                ..Options::new(CommandData::default)
            }),
        }
    }

    pub async fn get(&self, name: &str) -> Result<Option<CommandInfo>, StateError> {
        let data = self.state.get().await?;
        Ok(data.commands.get(name).map(info))
    }

    pub async fn list(&self) -> Result<Vec<CommandInfo>, StateError> {
        let data = self.state.get().await?;
        Ok(data.commands.values().map(info).collect())
    }

    pub async fn execute(
        &self,
        name: &str,
        invocation: Invocation,
        runtime: &dyn CommandRuntime,
    ) -> Result<(), CommandError> {
        let definition = match self.state.get().await {
            Ok(data) => data.commands.get(name).cloned(),
            Err(error) => {
                return Err(CommandError::Execution { command: name.to_string(), message: error.to_string() });
            }
        };
        let Some(definition) = definition else {
            return Err(CommandError::NotFound {
                command: name.to_string(),
                message: format!("Command not found: {name}"),
            });
        };
        match definition.execute.execute(invocation, runtime).await {
            Ok(()) => Ok(()),
            Err(message) => {
                log::error!("command execution failed: command={name} message={message}");
                Err(CommandError::Execution { command: name.to_string(), message })
            }
        }
    }

    pub async fn transform<F>(&self, f: F) -> Registration
    where
        F: for<'a, 'b> Fn(&'a mut CommandEditor<'b>) + Send + Sync + 'static,
    {
        self.state
            .transform(move |data| {
                let mut editor = CommandEditor::new(data);
                f(&mut editor);
                Ok(())
            })
            .await
    }

    pub async fn reload(&self) {
        self.state.reload().await;
    }
}

fn info(definition: &CommandDefinition) -> CommandInfo {
    CommandInfo { name: definition.name.clone(), description: definition.description.clone() }
}

fn projection(data: &CommandData) -> String {
    let mut out = String::new();
    for definition in data.commands.values() {
        info(definition).encode(&mut out);
    }
    out
}
