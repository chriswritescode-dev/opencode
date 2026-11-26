use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use opencode_contract::generated::{AgentID, ModelRef, ProviderID, SessionID};

use super::BoxFuture;
use super::http::{BufferedResponse, PreparedHttp};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    Primary,
    Compaction,
    Title,
    Generate,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelRequest {
    pub session_id: SessionID,
    pub agent: AgentID,
    pub model: ModelRef,
    pub kind: RequestKind,
    pub base_url: Option<String>,
    pub headers: IndexMap<String, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelResolve {
    pub session_id: SessionID,
    pub agent: AgentID,
    pub kind: RequestKind,
    pub model: ModelRef,
}

#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub session_id: SessionID,
    pub agent: AgentID,
    pub model: ModelRef,
    pub kind: RequestKind,
    pub request: PreparedHttp,
}

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub session_id: SessionID,
    pub agent: AgentID,
    pub model: ModelRef,
    pub kind: RequestKind,
    pub request: PreparedHttp,
    pub response: BufferedResponse,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WsHandshake {
    pub session_id: SessionID,
    pub agent: AgentID,
    pub model: ModelRef,
    pub kind: RequestKind,
    pub url: String,
    pub headers: IndexMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    Stop,
    Retry { delay_ms: u64 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Retry {
    pub session_id: SessionID,
    pub agent: AgentID,
    pub model: ModelRef,
    pub error: serde_json::Value,
    pub attempt: u32,
    pub decision: RetryDecision,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompactionResult {
    pub summary: String,
    pub native: Option<serde_json::Value>,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Compaction {
    pub session_id: SessionID,
    pub agent: AgentID,
    pub model: ModelRef,
    pub kind: RequestKind,
    pub options: IndexMap<String, serde_json::Value>,
    pub result: Option<CompactionResult>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TitleMessage {
    pub role: String,
    pub content: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Title {
    pub session_id: SessionID,
    pub agent: AgentID,
    pub model: ModelRef,
    pub kind: RequestKind,
    pub system: Vec<String>,
    pub messages: Vec<TitleMessage>,
    pub max_tokens: Option<f64>,
    pub result: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Context {
    pub session_id: SessionID,
    pub agent: AgentID,
    pub model: ModelRef,
    pub kind: RequestKind,
    pub options: IndexMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PromptEvent {
    pub session_id: SessionID,
}

type Handler<E> = Arc<dyn Fn(E) -> BoxFuture<E> + Send + Sync>;

struct Entry<E> {
    id: u64,
    provider: Option<ProviderID>,
    handler: Handler<E>,
}

pub struct HookRegistration {
    remove: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl HookRegistration {
    pub(crate) fn on_drop(remove: impl FnOnce() + Send + Sync + 'static) -> Self {
        Self { remove: Some(Box::new(remove)) }
    }
}

impl Drop for HookRegistration {
    fn drop(&mut self) {
        if let Some(remove) = self.remove.take() {
            remove();
        }
    }
}

struct Registry<E> {
    next: AtomicU64,
    entries: Arc<Mutex<Vec<Entry<E>>>>,
}

impl<E: 'static> Registry<E> {
    fn new() -> Self {
        Self { next: AtomicU64::new(0), entries: Arc::new(Mutex::new(Vec::new())) }
    }

    fn on(&self, provider: Option<ProviderID>, handler: Handler<E>) -> HookRegistration {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        opencode_util::lock(&self.entries).push(Entry { id, provider, handler });
        let entries = self.entries.clone();
        HookRegistration {
            remove: Some(Box::new(move || {
                opencode_util::lock(&entries).retain(|entry| entry.id != id);
            })),
        }
    }

    async fn trigger(&self, provider: &ProviderID, mut event: E) -> E {
        let handlers: Vec<Handler<E>> = opencode_util::lock(&self.entries)
            .iter()
            .filter(|entry| entry.provider.as_ref().is_none_or(|id| id == provider))
            .map(|entry| entry.handler.clone())
            .collect();
        for handler in handlers {
            event = handler(event).await;
        }
        event
    }
}

pub struct ProviderHooks {
    model_request: Registry<ModelRequest>,
    model_resolve: Registry<ModelResolve>,
    http_request: Registry<HttpRequest>,
    http_response: Registry<HttpResponse>,
    ws_handshake: Registry<WsHandshake>,
    retry: Registry<Retry>,
    context: Registry<Context>,
    compaction: Registry<Compaction>,
    title: Registry<Title>,
    prompt: Registry<PromptEvent>,
}

impl ProviderHooks {
    pub fn new() -> Self {
        Self {
            model_request: Registry::new(),
            model_resolve: Registry::new(),
            http_request: Registry::new(),
            http_response: Registry::new(),
            ws_handshake: Registry::new(),
            retry: Registry::new(),
            context: Registry::new(),
            compaction: Registry::new(),
            title: Registry::new(),
            prompt: Registry::new(),
        }
    }

    pub fn on_model_request(
        &self,
        provider: Option<ProviderID>,
        handler: impl Fn(ModelRequest) -> BoxFuture<ModelRequest> + Send + Sync + 'static,
    ) -> HookRegistration {
        self.model_request.on(provider, Arc::new(handler))
    }

    pub async fn trigger_model_request(&self, event: ModelRequest) -> ModelRequest {
        let provider = event.model.provider_id.clone();
        self.model_request.trigger(&provider, event).await
    }

    pub fn on_model_resolve(
        &self,
        provider: Option<ProviderID>,
        handler: impl Fn(ModelResolve) -> BoxFuture<ModelResolve> + Send + Sync + 'static,
    ) -> HookRegistration {
        self.model_resolve.on(provider, Arc::new(handler))
    }

    pub async fn trigger_model_resolve(&self, event: ModelResolve) -> ModelResolve {
        let provider = event.model.provider_id.clone();
        self.model_resolve.trigger(&provider, event).await
    }

    pub fn on_http_request(
        &self,
        provider: Option<ProviderID>,
        handler: impl Fn(HttpRequest) -> BoxFuture<HttpRequest> + Send + Sync + 'static,
    ) -> HookRegistration {
        self.http_request.on(provider, Arc::new(handler))
    }

    pub async fn trigger_http_request(&self, event: HttpRequest) -> HttpRequest {
        let provider = event.model.provider_id.clone();
        self.http_request.trigger(&provider, event).await
    }

    pub fn on_http_response(
        &self,
        provider: Option<ProviderID>,
        handler: impl Fn(HttpResponse) -> BoxFuture<HttpResponse> + Send + Sync + 'static,
    ) -> HookRegistration {
        self.http_response.on(provider, Arc::new(handler))
    }

    pub async fn trigger_http_response(&self, event: HttpResponse) -> HttpResponse {
        let provider = event.model.provider_id.clone();
        self.http_response.trigger(&provider, event).await
    }

    pub fn on_ws_handshake(
        &self,
        provider: Option<ProviderID>,
        handler: impl Fn(WsHandshake) -> BoxFuture<WsHandshake> + Send + Sync + 'static,
    ) -> HookRegistration {
        self.ws_handshake.on(provider, Arc::new(handler))
    }

    pub async fn trigger_ws_handshake(&self, event: WsHandshake) -> WsHandshake {
        let provider = event.model.provider_id.clone();
        self.ws_handshake.trigger(&provider, event).await
    }

    pub fn on_retry(
        &self,
        provider: Option<ProviderID>,
        handler: impl Fn(Retry) -> BoxFuture<Retry> + Send + Sync + 'static,
    ) -> HookRegistration {
        self.retry.on(provider, Arc::new(handler))
    }

    pub async fn trigger_retry(&self, event: Retry) -> Retry {
        let provider = event.model.provider_id.clone();
        self.retry.trigger(&provider, event).await
    }

    pub fn on_context(
        &self,
        provider: Option<ProviderID>,
        handler: impl Fn(Context) -> BoxFuture<Context> + Send + Sync + 'static,
    ) -> HookRegistration {
        self.context.on(provider, Arc::new(handler))
    }

    pub async fn trigger_context(&self, event: Context) -> Context {
        let provider = event.model.provider_id.clone();
        self.context.trigger(&provider, event).await
    }

    pub fn on_compaction(
        &self,
        provider: Option<ProviderID>,
        handler: impl Fn(Compaction) -> BoxFuture<Compaction> + Send + Sync + 'static,
    ) -> HookRegistration {
        self.compaction.on(provider, Arc::new(handler))
    }

    pub async fn trigger_compaction(&self, event: Compaction) -> Compaction {
        let provider = event.model.provider_id.clone();
        self.compaction.trigger(&provider, event).await
    }

    pub fn on_title(
        &self,
        provider: Option<ProviderID>,
        handler: impl Fn(Title) -> BoxFuture<Title> + Send + Sync + 'static,
    ) -> HookRegistration {
        self.title.on(provider, Arc::new(handler))
    }

    pub async fn trigger_title(&self, event: Title) -> Title {
        let provider = event.model.provider_id.clone();
        self.title.trigger(&provider, event).await
    }

    pub fn on_prompt(
        &self,
        handler: impl Fn(PromptEvent) -> BoxFuture<PromptEvent> + Send + Sync + 'static,
    ) -> HookRegistration {
        self.prompt.on(None, Arc::new(handler))
    }

    pub async fn trigger_prompt(&self, event: PromptEvent) -> PromptEvent {
        self.prompt.trigger(&ProviderID(String::new()), event).await
    }
}

impl Default for ProviderHooks {
    fn default() -> Self {
        Self::new()
    }
}
