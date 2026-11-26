use std::collections::HashMap;
use std::fmt;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use opencode_ai::AiError;
use opencode_bus::{Bus, BusError, Definition, PublishOptions};
use opencode_catalog::model::ModelService;
use opencode_catalog::resolver::Resolver;
use opencode_contract::codec::{Encode, encode_json};
use opencode_contract::generated::{
    AbsolutePath, LocationRef, SessionExecutionFailedData, SessionExecutionInterruptedData,
    SessionExecutionInterruptedDataReason, SessionExecutionStartedData, SessionExecutionSucceededData, SessionID,
    WorkspaceID,
};
use opencode_registry::agent::AgentService;
use rusqlite::Connection;
use serde_json::Value;
use tokio::sync::watch;

use crate::context::ContextError;
use crate::instructions::BuiltinsInput;
use crate::runner::Runner;
use crate::store::{claim_in, release_in};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterruptReason {
    User,
    Shutdown,
}

impl InterruptReason {
    pub fn name(self) -> &'static str {
        match self {
            InterruptReason::User => "user",
            InterruptReason::Shutdown => "shutdown",
        }
    }
}

pub trait SessionExecution: Send + Sync {
    fn wake(&self, session_id: &str);
    fn interrupt<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, ()>;
    fn wait_idle<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, ()>;
    fn active(&self) -> Vec<String>;
    fn shutdown<'a>(&'a self) -> BoxFuture<'a, ()>;
}

pub struct NoopExecution;

impl SessionExecution for NoopExecution {
    fn wake(&self, _session_id: &str) {}

    fn interrupt<'a>(&'a self, _session_id: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn wait_idle<'a>(&'a self, _session_id: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn active(&self) -> Vec<String> {
        Vec::new()
    }

    fn shutdown<'a>(&'a self) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

pub trait Placed: Send + Sync {
    fn models(&self) -> &ModelService;
    fn resolver(&self) -> &Resolver;
    fn agents(&self) -> &AgentService;
    fn builtins(&self) -> &BuiltinsInput;
    fn location(&self) -> Option<&LocationRef>;
    fn is_closed(&self) -> bool;
}

pub trait Placement: Send + Sync {
    fn place(&self, session_id: String) -> BoxFuture<'static, Result<Arc<dyn Placed>, String>>;
}

#[derive(Debug)]
pub enum ExecutionError {
    Context(ContextError),
    Ai(AiError),
    OutputLimit,
    Interrupted,
    Placement(String),
    Message(String),
    Bus(String),
}

impl fmt::Display for ExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExecutionError::Context(error) => formatter.write_str(&error.to_string()),
            ExecutionError::Ai(error) => formatter.write_str(error.message()),
            ExecutionError::OutputLimit => {
                formatter.write_str("Response still truncated after two output token limit continuations")
            }
            ExecutionError::Interrupted => formatter.write_str("Step interrupted"),
            ExecutionError::Placement(message) | ExecutionError::Message(message) | ExecutionError::Bus(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl std::error::Error for ExecutionError {}

pub fn contract_location(location: &opencode_location::location::LocationRef) -> LocationRef {
    LocationRef {
        directory: AbsolutePath(location.directory.clone()),
        workspace_id: location.workspace_id.clone().map(WorkspaceID),
    }
}

pub struct Execution {
    state: Arc<State>,
}

struct State {
    bus: Arc<Bus>,
    runner: Arc<Runner>,
    active: Mutex<HashMap<String, Arc<Active>>>,
    shutting_down: AtomicBool,
}

struct Active {
    cancel: watch::Sender<bool>,
    done: watch::Sender<bool>,
    pending: AtomicBool,
    stopping: AtomicBool,
    reason: Mutex<Option<InterruptReason>>,
}

enum Terminal {
    Succeeded,
    Failed(Box<ExecutionError>),
    Interrupted(InterruptReason),
}

impl Execution {
    pub fn new(bus: Arc<Bus>, runner: Arc<Runner>) -> Arc<Self> {
        Arc::new(Self {
            state: Arc::new(State {
                bus,
                runner,
                active: Mutex::new(HashMap::new()),
                shutting_down: AtomicBool::new(false),
            }),
        })
    }
}

impl SessionExecution for Execution {
    fn wake(&self, session_id: &str) {
        self.state.wake(session_id);
    }

    fn interrupt<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move { self.state.interrupt_with(session_id, InterruptReason::User).await })
    }

    fn wait_idle<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move { self.state.wait_idle(session_id).await })
    }

    fn active(&self) -> Vec<String> {
        self.state.active()
    }

    fn shutdown<'a>(&'a self) -> BoxFuture<'a, ()> {
        Box::pin(async move { self.state.shutdown().await })
    }
}

impl State {
    fn start(self: &Arc<Self>, session_id: &str) {
        let active = {
            let mut map = self.active.lock().expect("the execution map lock is never poisoned");
            if self.shutting_down.load(Ordering::SeqCst) {
                return;
            }
            match map.entry(session_id.to_owned()) {
                std::collections::hash_map::Entry::Occupied(entry) => {
                    entry.get().pending.store(true, Ordering::SeqCst);
                    return;
                }
                std::collections::hash_map::Entry::Vacant(entry) => entry.insert(Arc::new(Active::new())).clone(),
            }
        };
        let state = self.clone();
        let id = session_id.to_owned();
        tokio::spawn(async move { state.run(id, active).await });
    }

    fn wake(self: &Arc<Self>, session_id: &str) {
        self.start(session_id);
    }

    async fn run(self: Arc<Self>, session_id: String, active: Arc<Active>) {
        let terminal = match self.publish_started(&session_id) {
            Ok(()) => self.busy(&session_id, &active).await,
            Err(error) => Terminal::Failed(Box::new(ExecutionError::Bus(error.to_string()))),
        };
        self.settle(&session_id, &active, terminal).await;
    }

    async fn busy(&self, session_id: &str, active: &Active) -> Terminal {
        loop {
            let cancel = active.cancel.subscribe();
            if *cancel.borrow() {
                return Terminal::Interrupted(interrupt_reason(active));
            }
            let drained = AssertUnwindSafe(self.runner.drain(session_id, cancel)).catch_unwind().await;
            match drained {
                Ok(Ok(())) => {
                    if *active.cancel.borrow() {
                        return Terminal::Interrupted(interrupt_reason(active));
                    }
                    if active.pending.swap(false, Ordering::SeqCst) {
                        continue;
                    }
                    return Terminal::Succeeded;
                }
                Ok(Err(ExecutionError::Interrupted)) => return Terminal::Interrupted(interrupt_reason(active)),
                Ok(Err(error)) => return Terminal::Failed(Box::new(error)),
                Err(_) => {
                    log::error!("session drain panicked for {session_id}");
                    return Terminal::Failed(Box::new(ExecutionError::Message("session drain panicked".to_owned())));
                }
            }
        }
    }

    async fn settle(self: &Arc<Self>, session_id: &str, active: &Arc<Active>, terminal: Terminal) {
        let id = session_id.to_owned();
        let (definition, value, release) = match terminal {
            Terminal::Succeeded => (
                Definition::latest_durable("session.execution.succeeded")
                    .expect("session.execution.succeeded is durable"),
                json_value(&SessionExecutionSucceededData { session_id: SessionID(id) }),
                true,
            ),
            Terminal::Failed(error) => (
                Definition::latest_durable("session.execution.failed").expect("session.execution.failed is durable"),
                json_value(&SessionExecutionFailedData {
                    session_id: SessionID(id),
                    error: crate::retry::structured(&error),
                }),
                true,
            ),
            Terminal::Interrupted(reason) => (
                Definition::latest_durable("session.execution.interrupted")
                    .expect("session.execution.interrupted is durable"),
                json_value(&SessionExecutionInterruptedData {
                    session_id: SessionID(id),
                    reason: match reason {
                        InterruptReason::User => SessionExecutionInterruptedDataReason::User,
                        InterruptReason::Shutdown => SessionExecutionInterruptedDataReason::Shutdown,
                    },
                }),
                reason != InterruptReason::Shutdown,
            ),
        };
        let mut options = PublishOptions::default();
        if release {
            let id = session_id.to_owned();
            options.commit = Some(Box::new(move |connection: &Connection, _seq: i64| {
                release_in(connection, &id).map_err(BusError::from)
            }));
        }
        if let Err(error) = self.bus.publish(&definition, value, options) {
            log::error!("failed to publish {} for {session_id}: {error}", definition.r#type);
        }
        let successor = {
            let mut map = self.active.lock().expect("the execution map lock is never poisoned");
            let successor = (active.pending.swap(false, Ordering::SeqCst)
                && !self.shutting_down.load(Ordering::SeqCst))
            .then(|| Arc::new(Active::new()));
            if map.get(session_id).is_some_and(|current| Arc::ptr_eq(current, active)) {
                map.remove(session_id);
            }
            if let Some(successor) = &successor {
                map.insert(session_id.to_owned(), successor.clone());
            }
            successor
        };
        if let Some(successor) = successor {
            self.spawn(session_id.to_owned(), successor);
        }
        let _ = active.done.send_replace(true);
    }

    fn spawn(self: &Arc<Self>, session_id: String, active: Arc<Active>) {
        let state = self.clone();
        tokio::spawn(async move { state.run(session_id, active).await });
    }

    fn publish_started(&self, session_id: &str) -> Result<(), BusError> {
        let id = session_id.to_owned();
        let options = PublishOptions {
            commit: Some(Box::new(move |connection: &Connection, _seq: i64| {
                claim_in(connection, &id).map_err(BusError::from)
            })),
            ..Default::default()
        };
        let definition =
            Definition::latest_durable("session.execution.started").expect("session.execution.started is durable");
        self.bus
            .publish(
                &definition,
                json_value(&SessionExecutionStartedData { session_id: SessionID(session_id.to_owned()) }),
                options,
            )
            .map(|_| ())
    }

    async fn interrupt_with(&self, session_id: &str, reason: InterruptReason) {
        let active = self.active.lock().expect("the execution map lock is never poisoned").get(session_id).cloned();
        let Some(active) = active else {
            return;
        };
        if !active.stopping.swap(true, Ordering::SeqCst) {
            *active.reason.lock().expect("the interrupt reason lock is never poisoned") = Some(reason);
            active.pending.store(false, Ordering::SeqCst);
            active.cancel.send_replace(true);
        }
        let mut done = active.done.subscribe();
        let _ = done.wait_for(|value| *value).await;
    }

    async fn wait_idle(&self, session_id: &str) {
        loop {
            let active = self.active.lock().expect("the execution map lock is never poisoned").get(session_id).cloned();
            let Some(active) = active else {
                return;
            };
            let mut done = active.done.subscribe();
            let _ = done.wait_for(|value| *value).await;
        }
    }

    fn active(&self) -> Vec<String> {
        self.active.lock().expect("the execution map lock is never poisoned").keys().cloned().collect()
    }

    async fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        loop {
            let ids = self.active();
            if ids.is_empty() {
                return;
            }
            for id in ids {
                self.interrupt_with(&id, InterruptReason::Shutdown).await;
            }
        }
    }
}

impl Active {
    fn new() -> Active {
        Active {
            cancel: watch::channel(false).0,
            done: watch::channel(false).0,
            pending: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            reason: Mutex::new(None),
        }
    }
}

fn interrupt_reason(active: &Active) -> InterruptReason {
    active.reason.lock().expect("the interrupt reason lock is never poisoned").unwrap_or(InterruptReason::Shutdown)
}

fn json_value<T: Encode + ?Sized>(value: &T) -> Value {
    serde_json::from_str(&encode_json(value)).expect("a contract event encodes as JSON")
}
