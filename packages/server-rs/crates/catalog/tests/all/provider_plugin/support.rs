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
    AbsolutePath, AgentID, LocationRef, ModelRef, ProviderID, ProviderInfo, ProviderSettings, SessionID, WorkspaceID,
};
use serde_json::Value;

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

#[derive(Clone)]
pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn json(value: Value) -> Self {
        Self::json_status(value, 200)
    }

    pub fn json_status(value: Value, status: u16) -> Self {
        Self { status, body: serde_json::to_vec(&value).expect("a route reply serializes") }
    }

    pub fn empty() -> Self {
        Self { status: 200, body: Vec::new() }
    }

    pub fn status(status: u16) -> Self {
        Self { status, body: Vec::new() }
    }
}

struct Route {
    method: String,
    url: String,
    body_contains: Option<String>,
    respond: Box<dyn Fn(&PreparedHttp) -> Reply + Send + Sync>,
}

pub struct Routes {
    routes: Arc<Mutex<Vec<Route>>>,
    requests: Arc<Mutex<Vec<PreparedHttp>>>,
    held: Arc<Mutex<Vec<(String, String)>>>,
    arrived: Arc<tokio::sync::Semaphore>,
    release: Arc<tokio::sync::Semaphore>,
}

impl Routes {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            routes: Arc::new(Mutex::new(Vec::new())),
            requests: Arc::new(Mutex::new(Vec::new())),
            held: Arc::new(Mutex::new(Vec::new())),
            arrived: Arc::new(tokio::sync::Semaphore::new(0)),
            release: Arc::new(tokio::sync::Semaphore::new(0)),
        })
    }

    pub fn route_json(&self, method: &str, url: &str, value: Value) {
        self.route_with(method, url, move |_| Reply::json(value.clone()));
    }

    pub fn route_json_for(&self, method: &str, url: &str, body_contains: &str, value: Value) {
        self.routes.lock().expect("the routed replies lock is never poisoned").push(Route {
            method: method.to_owned(),
            url: url.to_owned(),
            body_contains: Some(body_contains.to_owned()),
            respond: Box::new(move |_| Reply::json(value.clone())),
        });
    }

    pub fn route_status_for(&self, method: &str, url: &str, body_contains: &str, status: u16) {
        self.routes.lock().expect("the routed replies lock is never poisoned").push(Route {
            method: method.to_owned(),
            url: url.to_owned(),
            body_contains: Some(body_contains.to_owned()),
            respond: Box::new(move |_| Reply::status(status)),
        });
    }

    pub fn route_with(
        &self,
        method: &str,
        url: &str,
        respond: impl Fn(&PreparedHttp) -> Reply + Send + Sync + 'static,
    ) {
        self.routes.lock().expect("the routed replies lock is never poisoned").push(Route {
            method: method.to_owned(),
            url: url.to_owned(),
            body_contains: None,
            respond: Box::new(respond),
        });
    }

    pub fn set_json(&self, method: &str, url: &str, value: Value) {
        self.remove(method, url);
        self.route_json(method, url, value);
    }

    pub fn set_status(&self, method: &str, url: &str, status: u16) {
        self.remove(method, url);
        self.route_with(method, url, move |_| Reply::status(status));
    }

    pub fn remove(&self, method: &str, url: &str) {
        self.routes
            .lock()
            .expect("the routed replies lock is never poisoned")
            .retain(|route| !(route.method.eq_ignore_ascii_case(method) && route.url == url));
    }

    pub fn requests(&self) -> Vec<PreparedHttp> {
        self.requests.lock().expect("the routed requests lock is never poisoned").clone()
    }

    pub fn count(&self, method: &str, url: &str) -> usize {
        self.requests()
            .iter()
            .filter(|request| request.method.eq_ignore_ascii_case(method) && request.url == url)
            .count()
    }

    pub fn hold(&self, method: &str, url: &str) {
        self.held.lock().expect("the routed holds lock is never poisoned").push((method.to_owned(), url.to_owned()));
    }

    pub async fn arrived(&self) {
        let _permit = self.arrived.acquire().await;
    }

    pub fn release(&self) {
        self.release.add_permits(1);
    }
}

impl HttpSender for Routes {
    fn send(&self, request: PreparedHttp) -> BoxFuture<Result<HttpResponse, AiError>> {
        self.requests.lock().expect("the routed requests lock is never poisoned").push(request.clone());
        let routes = self.routes.clone();
        let held = self.held.clone();
        let arrived = self.arrived.clone();
        let release = self.release.clone();
        Box::pin(async move {
            let hold = held
                .lock()
                .expect("the routed holds lock is never poisoned")
                .iter()
                .any(|(method, url)| method.eq_ignore_ascii_case(&request.method) && *url == request.url);
            if hold {
                arrived.add_permits(1);
                let _permit = release.acquire().await;
            }
            let reply = {
                let routes = routes.lock().expect("the routed replies lock is never poisoned");
                routes
                    .iter()
                    .find(|route| {
                        route.method.eq_ignore_ascii_case(&request.method)
                            && route.url == request.url
                            && route
                                .body_contains
                                .as_ref()
                                .is_none_or(|needle| String::from_utf8_lossy(&request.body).contains(needle.as_str()))
                    })
                    .map(|route| (route.respond)(&request))
            };
            let Some(reply) = reply else {
                return Err(AiError {
                    reason: AiErrorReason::Transport {
                        message: format!("no route for {}", request.url),
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
                status: reply.status,
                headers: vec![("content-type".to_owned(), "application/json".to_owned())],
                body: Box::pin(futures_util::stream::once(async move { Ok(bytes::Bytes::from(reply.body)) })),
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

pub fn plugin_context_at(harness: &Harness, http: Arc<dyn HttpSender>, workspace_id: Option<&str>) -> PluginContext {
    plugin_context_with_runner(harness, http, Arc::new(FakeRunner::default()), workspace_id)
}

pub fn plugin_context_with_runner(
    harness: &Harness,
    http: Arc<dyn HttpSender>,
    runner: Arc<dyn CommandRunner>,
    workspace_id: Option<&str>,
) -> PluginContext {
    let environment = harness.environment.clone();
    PluginContext {
        http,
        app: AppInfo { name: "opencode".to_owned(), version: "test".to_owned(), channel: "test".to_owned() },
        env: Arc::new(move |name| environment.get(name).map(OsString::from)),
        kv: Arc::new(NoKv),
        commands: runner,
        sessions: Arc::new(FakeSessions::default()),
        hooks: Arc::new(ProviderHooks::default()),
        location: LocationRef {
            directory: AbsolutePath(harness.root().to_string_lossy().into_owned()),
            workspace_id: workspace_id.map(|id| WorkspaceID(id.to_owned())),
        },
        shared_cache: opencode_catalog::shared_cache::SharedCache::new(),
        gitlab_discovery: Arc::new(opencode_catalog::provider_plugin::gitlab::NoDiscovery),
    }
}

pub fn plugin_context_with(harness: &Harness, http: Arc<dyn HttpSender>) -> PluginContext {
    plugin_context_at(harness, http, None)
}

pub fn plugin_context(harness: &Harness) -> PluginContext {
    plugin_context_at(harness, Arc::new(RecordedSender::default()), None)
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

pub fn provider_settings_from(value: Value) -> ProviderSettings {
    decode_json(&value.to_string()).expect("the provider settings decode")
}

pub fn settings_json(provider: &ProviderInfo) -> Option<Value> {
    provider.settings.as_ref().map(|settings| {
        serde_json::from_str(&opencode_contract::codec::encode_json(settings)).expect("the provider settings json")
    })
}

pub async fn wait_for_setting(harness: &Harness, id: &str, key: &str, value: &str) -> ProviderInfo {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let provider = harness.services.provider.get(&ProviderID(id.to_owned())).await.unwrap().unwrap();
            let matches = settings_json(&provider)
                .and_then(|settings| settings.get(key).and_then(Value::as_str).map(|current| current == value))
                .unwrap_or(false);
            if matches {
                return provider;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the provider setting settles")
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
