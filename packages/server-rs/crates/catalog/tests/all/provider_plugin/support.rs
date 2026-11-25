#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::io;
use std::sync::{Arc, Mutex};

use opencode_ai::route::executor::{BoxFuture, HttpResponse, HttpSender, PreparedHttp};
use opencode_ai::schema::{AiError, AiErrorReason, TransportKind, TransportOperation};
use opencode_catalog::provider_plugin::hooks::ProviderHooks;
use opencode_catalog::provider_plugin::{
    AppInfo, CommandOutput, CommandRunner, PluginContext, SessionAccess, SessionRef, UserMessage,
};
use opencode_config::seams::NoKv;
use opencode_contract::codec::{JsonMap, decode_json};
use opencode_contract::generated::{
    AbsolutePath, AgentID, LocationRef, ModelRef, ProviderID, ProviderInfo, ProviderSettings, SessionID,
};

use crate::builtin::Harness;

#[derive(Debug, Clone, PartialEq)]
pub struct Exchange {
    pub method: String,
    pub url: String,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Exchange {
    pub fn new(method: &str, url: &str, status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self { method: method.to_owned(), url: url.to_owned(), status, headers: Vec::new(), body: body.into() }
    }
}

pub struct RecordedSender {
    keyed: bool,
    exchanges: Mutex<VecDeque<Exchange>>,
    requests: Mutex<Vec<PreparedHttp>>,
}

impl RecordedSender {
    pub fn ordered(exchanges: Vec<Exchange>) -> Self {
        Self { keyed: false, exchanges: Mutex::new(exchanges.into()), requests: Mutex::new(Vec::new()) }
    }

    pub fn keyed(exchanges: Vec<Exchange>) -> Self {
        Self { keyed: true, exchanges: Mutex::new(exchanges.into()), requests: Mutex::new(Vec::new()) }
    }

    pub fn requests(&self) -> Vec<PreparedHttp> {
        self.requests.lock().expect("the recorded requests lock is never poisoned").clone()
    }
}

impl Default for RecordedSender {
    fn default() -> Self {
        Self::ordered(Vec::new())
    }
}

impl HttpSender for RecordedSender {
    fn send(&self, request: PreparedHttp) -> BoxFuture<Result<HttpResponse, AiError>> {
        self.requests.lock().expect("the recorded requests lock is never poisoned").push(request.clone());
        let exchange = {
            let mut exchanges = self.exchanges.lock().expect("the recorded exchanges lock is never poisoned");
            if self.keyed {
                let index = exchanges.iter().position(|exchange| {
                    exchange.url == request.url && exchange.method.eq_ignore_ascii_case(&request.method)
                });
                index.and_then(|index| exchanges.remove(index))
            } else {
                exchanges.pop_front()
            }
        };
        Box::pin(async move {
            let Some(exchange) = exchange else {
                return Err(AiError {
                    reason: AiErrorReason::Transport {
                        message: format!("no recorded exchange for {}", request.url),
                        body: None,
                        http: None,
                        transport: TransportKind::Http,
                        operation: TransportOperation::Request,
                        code: None,
                        url: Some(request.url),
                        phase: None,
                        delivery: None,
                        recovery: None,
                    },
                });
            };
            Ok(HttpResponse {
                status: exchange.status,
                headers: exchange.headers,
                body: Box::pin(futures_util::stream::once(async move { Ok(bytes::Bytes::from(exchange.body)) })),
            })
        })
    }
}

#[derive(Default)]
pub struct FakeSessions {
    pub sessions: HashMap<String, SessionRef>,
    pub messages: HashMap<String, UserMessage>,
    pub agents: HashMap<String, ModelRef>,
}

impl SessionAccess for FakeSessions {
    fn session(&self, id: SessionID) -> BoxFuture<Option<SessionRef>> {
        let value = self.sessions.get(&id.0).cloned();
        Box::pin(async move { value })
    }

    fn last_user_message(&self, session: SessionID) -> BoxFuture<Option<UserMessage>> {
        let value = self.messages.get(&session.0).cloned();
        Box::pin(async move { value })
    }

    fn agent_model(&self, agent: AgentID) -> BoxFuture<Option<ModelRef>> {
        let value = self.agents.get(&agent.0).cloned();
        Box::pin(async move { value })
    }
}

pub type RecordedCall = (String, Vec<String>, Vec<(OsString, OsString)>);

#[derive(Default)]
pub struct FakeRunner {
    outputs: Mutex<VecDeque<CommandOutput>>,
    calls: Mutex<Vec<RecordedCall>>,
}

impl FakeRunner {
    pub fn push(&self, output: CommandOutput) {
        self.outputs.lock().expect("the command outputs lock is never poisoned").push_back(output);
    }

    pub fn calls(&self) -> Vec<RecordedCall> {
        self.calls.lock().expect("the command calls lock is never poisoned").clone()
    }

    pub fn succeed(stdout: &str, stderr: &str) -> CommandOutput {
        CommandOutput {
            status: std::process::ExitStatus::default(),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }
}

impl CommandRunner for FakeRunner {
    fn run(
        &self,
        program: String,
        args: Vec<String>,
        env: Vec<(OsString, OsString)>,
    ) -> BoxFuture<io::Result<CommandOutput>> {
        self.calls.lock().expect("the command calls lock is never poisoned").push((program.clone(), args, env));
        let output = self.outputs.lock().expect("the command outputs lock is never poisoned").pop_front();
        Box::pin(async move {
            output.ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, format!("no recorded command output for {program}"))
            })
        })
    }
}

pub fn plugin_context(harness: &Harness) -> PluginContext {
    let environment = harness.environment.clone();
    PluginContext {
        http: Arc::new(RecordedSender::default()),
        app: AppInfo { name: "opencode".to_owned(), version: "test".to_owned(), channel: "test".to_owned() },
        env: Arc::new(move |name| environment.get(name).map(OsString::from)),
        kv: Arc::new(NoKv),
        commands: Arc::new(FakeRunner::default()),
        sessions: Arc::new(FakeSessions::default()),
        hooks: Arc::new(ProviderHooks::default()),
        location: LocationRef {
            directory: AbsolutePath(harness.root().to_string_lossy().into_owned()),
            workspace_id: None,
        },
        shared_cache: opencode_catalog::shared_cache::SharedCache::new(),
    }
}

pub fn headers(provider: &ProviderInfo) -> Option<HashMap<String, String>> {
    provider.headers.as_ref().map(|headers| headers.0.iter().cloned().collect())
}

pub fn base_url(provider: &ProviderInfo) -> Option<String> {
    let settings = provider.settings.as_ref()?;
    let (_, value) = settings.rest.0.iter().find(|(key, _)| key == "baseURL")?;
    value.as_str().map(str::to_owned)
}

pub fn provider_settings(base_url: &str) -> ProviderSettings {
    decode_json(&serde_json::json!({ "baseURL": base_url }).to_string()).expect("the provider settings decode")
}

pub async fn seed_provider(
    harness: &Harness,
    id: &str,
    package: &str,
    base_url: Option<&str>,
    headers: Option<Vec<(&str, &str)>>,
) -> opencode_location::state::Registration {
    let id = ProviderID(id.to_owned());
    let package = package.to_owned();
    let settings = base_url.map(provider_settings);
    let headers = headers
        .map(|headers| JsonMap(headers.into_iter().map(|(name, value)| (name.to_owned(), value.to_owned())).collect()));
    harness
        .services
        .provider
        .transform(move |editor| {
            editor.update(&id, |provider| {
                provider.package = package.clone();
                if let Some(settings) = settings.clone() {
                    provider.settings = Some(settings);
                }
                if let Some(headers) = headers.clone() {
                    provider.headers = Some(headers);
                }
            });
            Ok(())
        })
        .await
}
