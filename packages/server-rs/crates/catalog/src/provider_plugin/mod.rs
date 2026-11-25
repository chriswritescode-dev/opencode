pub mod cerebras;
pub mod hooks;
pub mod http;
pub mod neon;
pub mod prompt_cache_key;

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use opencode_contract::generated::{AgentID, LocationRef, ModelRef, SessionID};

use crate::integration::ConfigEnv;

pub type BoxFuture<T> = futures_util::future::BoxFuture<'static, T>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub channel: String,
}

impl AppInfo {
    pub fn useragent(&self) -> String {
        format!("opencode/{}/{}/{}", self.channel, self.version, self.name)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommandOutput {
    pub status: std::process::ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub trait CommandRunner: Send + Sync {
    fn run(
        &self,
        program: String,
        args: Vec<String>,
        env: Vec<(OsString, OsString)>,
    ) -> BoxFuture<io::Result<CommandOutput>>;
}

#[derive(Default, Debug, Clone, Copy)]
pub struct TokioCommandRunner;

impl CommandRunner for TokioCommandRunner {
    fn run(
        &self,
        program: String,
        args: Vec<String>,
        env: Vec<(OsString, OsString)>,
    ) -> BoxFuture<io::Result<CommandOutput>> {
        Box::pin(async move {
            let output = tokio::process::Command::new(program)
                .args(args)
                .envs(env)
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true)
                .output()
                .await?;
            Ok(CommandOutput { status: output.status, stdout: output.stdout, stderr: output.stderr })
        })
    }
}

pub fn which(program: &str, env: &ConfigEnv) -> Option<PathBuf> {
    let path = env("PATH")?;
    let extensions: Vec<OsString> = if cfg!(windows) {
        env("PATHEXT").map(|value| value.to_string_lossy().split(';').map(OsString::from).collect()).unwrap_or_default()
    } else {
        Vec::new()
    };
    std::env::split_paths(&path).find_map(|directory| {
        let candidate = directory.join(program);
        if candidate.is_file() {
            return Some(candidate);
        }
        extensions
            .iter()
            .map(|extension| {
                let mut name = OsString::from(program);
                name.push(extension);
                directory.join(name)
            })
            .find(|candidate| candidate.is_file())
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRef {
    pub id: SessionID,
    pub parent_id: Option<SessionID>,
    pub fork_session_id: Option<SessionID>,
}

impl SessionRef {
    pub fn affinity(&self) -> SessionID {
        self.parent_id.clone().or_else(|| self.fork_session_id.clone()).unwrap_or_else(|| self.id.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserMessage {
    pub text: String,
    pub has_image: bool,
}

pub trait SessionAccess: Send + Sync {
    fn session(&self, id: SessionID) -> BoxFuture<Option<SessionRef>>;
    fn last_user_message(&self, session: SessionID) -> BoxFuture<Option<UserMessage>>;
    fn agent_model(&self, agent: AgentID) -> BoxFuture<Option<ModelRef>>;
}

pub struct NoSessions;

impl SessionAccess for NoSessions {
    fn session(&self, _id: SessionID) -> BoxFuture<Option<SessionRef>> {
        Box::pin(async { None })
    }

    fn last_user_message(&self, _session: SessionID) -> BoxFuture<Option<UserMessage>> {
        Box::pin(async { None })
    }

    fn agent_model(&self, _agent: AgentID) -> BoxFuture<Option<ModelRef>> {
        Box::pin(async { None })
    }
}

pub struct PluginContext {
    pub http: Arc<dyn opencode_ai::HttpSender>,
    pub app: AppInfo,
    pub env: crate::integration::ConfigEnv,
    pub kv: Arc<dyn opencode_config::seams::KvStore>,
    pub commands: Arc<dyn CommandRunner>,
    pub sessions: Arc<dyn SessionAccess>,
    pub hooks: Arc<hooks::ProviderHooks>,
    pub location: LocationRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Native(&'static str),
    Elsewhere { id: &'static str, owner: &'static str },
}

impl Slot {
    pub fn id(&self) -> &'static str {
        match self {
            Self::Native(id) => id,
            Self::Elsewhere { id, .. } => id,
        }
    }
}

pub const ORDER: [Slot; 30] = [
    Slot::Native("opencode.provider.amazon.bedrock"),
    Slot::Native("opencode.provider.azure"),
    Slot::Native("opencode.provider.cerebras"),
    Slot::Native("opencode.provider.chatgpt"),
    Slot::Native("opencode.provider.cloudflare.ai.gateway"),
    Slot::Native("opencode.provider.cloudflare.workers.ai"),
    Slot::Native("opencode.provider.databricks"),
    Slot::Native("opencode.provider.digitalocean"),
    Slot::Native("opencode.provider.github.copilot"),
    Slot::Native("opencode.provider.gitlab"),
    Slot::Native("opencode.provider.google.vertex"),
    Slot::Native("opencode.provider.kilo"),
    Slot::Native("opencode.provider.llmgateway"),
    Slot::Native("opencode.provider.lmstudio"),
    Slot::Native("opencode.provider.modal"),
    Slot::Native("opencode.provider.neon"),
    Slot::Native("opencode.provider.nvidia"),
    Slot::Native("opencode.provider.ollama"),
    Slot::Elsewhere { id: "opencode.provider.opencode", owner: "OR-48" },
    Slot::Native("opencode.provider.snowflake.cortex"),
    Slot::Native("opencode.provider.openai"),
    Slot::Native("opencode.provider.openai-context-caps"),
    Slot::Native("opencode.provider.openrouter"),
    Slot::Native("opencode.provider.poe"),
    Slot::Native("opencode.provider.prompt-cache-key"),
    Slot::Elsewhere { id: "opencode.provider.sap.ai.core", owner: "OR-64" },
    Slot::Native("opencode.provider.vllm"),
    Slot::Native("opencode.provider.xai"),
    Slot::Native("opencode.provider.zenmux"),
    Slot::Elsewhere { id: "opencode.provider.claude-agent", owner: "OR-64" },
];

pub async fn register(
    services: &crate::builtin::Services,
    context: &PluginContext,
    builtin: &mut crate::builtin::Builtin,
) {
    for slot in ORDER {
        let Slot::Native(id) = slot else {
            continue;
        };
        match id {
            cerebras::ID => cerebras::register(services, context, builtin).await,
            prompt_cache_key::ID => prompt_cache_key::register(services, context, builtin).await,
            neon::ID => neon::register(services, context, builtin).await,
            _ => {}
        }
    }
}
