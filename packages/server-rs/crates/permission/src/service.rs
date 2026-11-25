use std::collections::HashMap;
use std::future::ready;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use futures_util::future::BoxFuture;
use indexmap::IndexMap;
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    AgentID, PermissionEffect, PermissionID, PermissionReply, PermissionRequest, PermissionRule, PermissionSource,
    SessionID,
};
use tokio::sync::oneshot;

use crate::rule::{evaluate, merge};
use crate::saved::SavedError;

#[derive(Clone, Debug, PartialEq)]
pub struct SessionRules {
    pub agent: Option<String>,
    pub permissions: Vec<PermissionRule>,
}

pub trait PermissionSessions: Send + Sync {
    fn get(&self, session_id: &str) -> Result<Option<SessionRules>, String>;
}

pub trait PermissionAgents: Send + Sync {
    fn resolve<'a>(&'a self, id: Option<&'a str>) -> BoxFuture<'a, Option<Vec<PermissionRule>>>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct PermissionEvaluation {
    pub session_id: SessionID,
    pub agent: Option<AgentID>,
    pub action: String,
    pub resources: Vec<String>,
    pub metadata: Option<JsonMap<serde_json::Value>>,
    pub source: Option<PermissionSource>,
    pub effect: PermissionEffect,
    pub message: Option<String>,
}

pub trait EvaluateHook: Send + Sync {
    fn evaluate<'a>(&'a self, event: PermissionEvaluation) -> BoxFuture<'a, PermissionEvaluation>;
}

pub struct NoEvaluate;

impl EvaluateHook for NoEvaluate {
    fn evaluate<'a>(&'a self, event: PermissionEvaluation) -> BoxFuture<'a, PermissionEvaluation> {
        Box::pin(ready(event))
    }
}

pub trait PermissionEvents: Send + Sync {
    fn asked(&self, request: &PermissionRequest);
    fn replied(&self, session_id: &SessionID, request_id: &PermissionID, reply: PermissionReply);
}

pub struct AssertInput {
    pub id: Option<PermissionID>,
    pub session_id: SessionID,
    pub agent: Option<AgentID>,
    pub action: String,
    pub resources: Vec<String>,
    pub save: Option<Vec<String>>,
    pub metadata: Option<JsonMap<serde_json::Value>>,
    pub source: Option<PermissionSource>,
    pub unattended: Option<Vec<PermissionRule>>,
}

pub struct ReplyInput {
    pub request_id: PermissionID,
    pub reply: PermissionReply,
    pub message: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AskResult {
    pub id: PermissionID,
    pub effect: PermissionEffect,
}

#[derive(Debug, thiserror::Error)]
pub enum PermissionError {
    #[error("Session not found: {0:?}")]
    SessionNotFound(SessionID),
    #[error("Session store error: {0}")]
    Sessions(String),
    #[error("Permission request not found: {0:?}")]
    NotFound(PermissionID),
    #[error("Duplicate pending permission ID: {0:?}")]
    Duplicate(PermissionID),
    #[error(transparent)]
    Saved(#[from] SavedError),
    #[error("Permission denied: {permission}")]
    Blocked { rules: Vec<PermissionRule>, permission: String, resources: Vec<String>, reason: Option<String> },
    #[error("{feedback}")]
    Corrected { feedback: String },
    #[error("Permission declined")]
    Declined,
}

pub struct PermissionDeps {
    pub sessions: Arc<dyn PermissionSessions>,
    pub agents: Arc<dyn PermissionAgents>,
    pub saved: Arc<crate::saved::PermissionSaved>,
    pub events: Arc<dyn PermissionEvents>,
    pub project_id: String,
}

enum Resolution {
    Allow,
    Reject { feedback: Option<String> },
}

struct Pending {
    request: PermissionRequest,
    agent: Option<AgentID>,
    deferred: Option<oneshot::Sender<Resolution>>,
}

struct Evaluation {
    effect: PermissionEffect,
    message: Option<String>,
    rules: Vec<PermissionRule>,
}

type PendingStore = Arc<Mutex<IndexMap<String, Pending>>>;

pub struct PermissionService {
    deps: PermissionDeps,
    hook: Arc<dyn EvaluateHook>,
    pending: PendingStore,
    parents: Arc<Mutex<HashMap<String, String>>>,
    closed: AtomicBool,
}

pub struct NestedGuard {
    parents: Arc<Mutex<HashMap<String, String>>>,
    call: String,
}

struct PendingGuard {
    pending: PendingStore,
    id: String,
}

impl PermissionService {
    pub fn new(deps: PermissionDeps) -> Self {
        Self {
            deps,
            hook: Arc::new(NoEvaluate),
            pending: Arc::new(Mutex::new(IndexMap::new())),
            parents: Arc::new(Mutex::new(HashMap::new())),
            closed: AtomicBool::new(false),
        }
    }

    pub fn with_evaluate_hook(mut self, hook: Arc<dyn EvaluateHook>) -> Self {
        self.hook = hook;
        self
    }

    pub async fn ask(&self, input: AssertInput) -> Result<AskResult, PermissionError> {
        if self.is_closed() {
            return Ok(AskResult { id: request_id(input.id.clone()), effect: PermissionEffect::Deny });
        }
        let result = self.evaluate_input(&input).await?;
        let value = self.request(&input, result.message);
        if result.effect == PermissionEffect::Ask {
            self.create(value.clone(), input.agent.clone())?;
        }
        Ok(AskResult { id: value.id, effect: result.effect })
    }

    pub async fn assert(&self, input: AssertInput) -> Result<(), PermissionError> {
        if self.is_closed() {
            return Err(PermissionError::Declined);
        }
        let result = self.evaluate_input(&input).await?;
        if result.effect == PermissionEffect::Deny {
            return Err(blocked(&input, result));
        }
        if result.effect == PermissionEffect::Allow {
            return Ok(());
        }
        let value = self.request(&input, result.message);
        let receiver = self.create(value.clone(), input.agent.clone())?;
        let _guard = PendingGuard { pending: Arc::clone(&self.pending), id: value.id.0 };
        match receiver.await {
            Ok(Resolution::Allow) => Ok(()),
            Ok(Resolution::Reject { feedback: Some(feedback) }) if !feedback.is_empty() => {
                Err(PermissionError::Corrected { feedback })
            }
            Ok(Resolution::Reject { .. }) | Err(_) => Err(PermissionError::Declined),
        }
    }

    pub async fn assert_not_denied(&self, input: AssertInput) -> Result<(), PermissionError> {
        let result = self.evaluate_input(&input).await?;
        if result.effect != PermissionEffect::Deny {
            return Ok(());
        }
        Err(blocked(&input, result))
    }

    pub async fn allows(&self, input: AssertInput) -> Result<bool, PermissionError> {
        if self.is_closed() {
            return Ok(false);
        }
        Ok(self.evaluate_input(&input).await?.effect == PermissionEffect::Allow)
    }

    pub async fn reply(&self, input: ReplyInput) -> Result<(), PermissionError> {
        let existing = {
            let pending = self.lock_pending();
            let Some(entry) = pending.get(input.request_id.0.as_str()) else {
                return Err(PermissionError::NotFound(input.request_id.clone()));
            };
            entry.request.clone()
        };
        self.deps.events.replied(&existing.session_id, &existing.id, input.reply);
        if input.reply == PermissionReply::Reject {
            return self.reject(&input, &existing);
        }
        self.accept(input.reply, existing).await
    }

    pub fn get(&self, id: &str) -> Option<PermissionRequest> {
        self.lock_pending().get(id).map(|entry| entry.request.clone())
    }

    pub fn for_session(&self, session_id: &str) -> Vec<PermissionRequest> {
        self.lock_pending()
            .values()
            .filter(|entry| entry.request.session_id.0.as_str() == session_id)
            .map(|entry| entry.request.clone())
            .collect()
    }

    pub fn list(&self) -> Vec<PermissionRequest> {
        self.lock_pending().values().map(|entry| entry.request.clone()).collect()
    }

    pub fn nested(&self, call: &str, parent: &str) -> NestedGuard {
        {
            let mut parents = self.parents.lock().unwrap_or_else(|error| error.into_inner());
            parents.insert(call.to_string(), parent.to_string());
        }
        NestedGuard { parents: Arc::clone(&self.parents), call: call.to_string() }
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let drained = {
            let mut pending = self.lock_pending();
            pending.drain(..).map(|(_, entry)| entry).collect::<Vec<_>>()
        };
        for entry in drained {
            self.deps.events.replied(&entry.request.session_id, &entry.request.id, PermissionReply::Reject);
            send_resolution(Some(entry), Resolution::Reject { feedback: None });
        }
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    fn lock_pending(&self) -> MutexGuard<'_, IndexMap<String, Pending>> {
        self.pending.lock().unwrap_or_else(|error| error.into_inner())
    }

    async fn configured(
        &self,
        session_id: &SessionID,
        agent: Option<&AgentID>,
    ) -> Result<Vec<PermissionRule>, PermissionError> {
        let Some(session) = self.deps.sessions.get(session_id.0.as_str()).map_err(PermissionError::Sessions)? else {
            return Err(PermissionError::SessionNotFound(session_id.clone()));
        };
        let target = agent.map(|agent| agent.0.as_str()).or(session.agent.as_deref());
        let agent_rules = self.deps.agents.resolve(target).await.unwrap_or_else(missing_agent_rules);
        Ok(merge(&[agent_rules.as_slice(), session.permissions.as_slice()]))
    }

    fn saved_rules(&self) -> Result<Vec<PermissionRule>, PermissionError> {
        let rows = self.deps.saved.list(Some(&self.deps.project_id)).map_err(SavedError::from)?;
        Ok(rows
            .into_iter()
            .map(|row| PermissionRule { action: row.action, resource: row.resource, effect: PermissionEffect::Allow })
            .collect())
    }

    async fn evaluate_input(&self, input: &AssertInput) -> Result<Evaluation, PermissionError> {
        let rules = self.configured(&input.session_id, input.agent.as_ref()).await?;
        if denied(input, rules.as_slice()) {
            return Ok(Evaluation { effect: PermissionEffect::Deny, message: None, rules });
        }
        let all = merge(&[rules.as_slice(), self.saved_rules()?.as_slice()]);
        let effect = if input
            .resources
            .iter()
            .any(|resource| evaluate(&input.action, resource, &[all.as_slice()]).effect == PermissionEffect::Ask)
        {
            PermissionEffect::Ask
        } else {
            PermissionEffect::Allow
        };
        let event = self
            .hook
            .evaluate(PermissionEvaluation {
                session_id: input.session_id.clone(),
                agent: input.agent.clone(),
                action: input.action.clone(),
                resources: input.resources.clone(),
                metadata: input.metadata.clone(),
                source: self.row(input.source.clone()),
                effect,
                message: None,
            })
            .await;
        if event.effect != PermissionEffect::Ask {
            return Ok(Evaluation { effect: event.effect, message: event.message, rules: all });
        }
        let Some(unattended) = input.unattended.as_deref() else {
            return Ok(Evaluation { effect: event.effect, message: event.message, rules: all });
        };
        let settled = merge(&[all.as_slice(), unattended]);
        let allowed = input
            .resources
            .iter()
            .all(|resource| evaluate(&input.action, resource, &[settled.as_slice()]).effect == PermissionEffect::Allow);
        if allowed {
            return Ok(Evaluation { effect: PermissionEffect::Allow, message: None, rules: all });
        }
        let message = format!("Permission {} needs approval, and nobody is present to give it", input.action);
        Ok(Evaluation { effect: PermissionEffect::Deny, message: Some(message), rules: all })
    }

    fn row(&self, source: Option<PermissionSource>) -> Option<PermissionSource> {
        let mut source = source?;
        let parent = {
            let parents = self.parents.lock().unwrap_or_else(|error| error.into_inner());
            parents.get(&source.id).cloned()
        };
        if let Some(parent) = parent {
            source.id = parent;
        }
        Some(source)
    }

    fn request(&self, input: &AssertInput, message: Option<String>) -> PermissionRequest {
        PermissionRequest {
            id: request_id(input.id.clone()),
            session_id: input.session_id.clone(),
            action: input.action.clone(),
            resources: input.resources.clone(),
            save: input.save.clone(),
            metadata: input
                .metadata
                .clone()
                .map(|map| JsonMap(map.0.into_iter().filter(|(_, value)| !value.is_null()).collect())),
            source: self.row(input.source.clone()),
            message,
        }
    }

    fn create(
        &self,
        request: PermissionRequest,
        agent: Option<AgentID>,
    ) -> Result<oneshot::Receiver<Resolution>, PermissionError> {
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self.lock_pending();
            if self.is_closed() {
                drop(pending);
                let _ = sender.send(Resolution::Reject { feedback: None });
                return Ok(receiver);
            }
            if pending.contains_key(request.id.0.as_str()) {
                return Err(PermissionError::Duplicate(request.id));
            }
            pending.insert(request.id.0.clone(), Pending { request: request.clone(), agent, deferred: Some(sender) });
        }
        self.deps.events.asked(&request);
        Ok(receiver)
    }

    fn reject(&self, input: &ReplyInput, existing: &PermissionRequest) -> Result<(), PermissionError> {
        let feedback = input.message.clone().filter(|message| !message.is_empty());
        let (primary, batch) = {
            let mut pending = self.lock_pending();
            let primary = pending.shift_remove(existing.id.0.as_str());
            let ids = pending
                .iter()
                .filter(|(_, entry)| {
                    entry.request.session_id == existing.session_id
                        && message_id(entry.request.source.as_ref()) == message_id(existing.source.as_ref())
                })
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            let batch = ids.iter().filter_map(|id| pending.shift_remove(id)).collect::<Vec<_>>();
            (primary, batch)
        };
        send_resolution(primary, Resolution::Reject { feedback: feedback.clone() });
        for entry in batch {
            self.deps.events.replied(&entry.request.session_id, &entry.request.id, PermissionReply::Reject);
            send_resolution(Some(entry), Resolution::Reject { feedback: feedback.clone() });
        }
        Ok(())
    }

    async fn accept(&self, reply: PermissionReply, existing: PermissionRequest) -> Result<(), PermissionError> {
        let save =
            (reply == PermissionReply::Always).then(|| existing.save.clone()).flatten().filter(|save| !save.is_empty());
        if let Some(save) = save.as_ref() {
            self.deps.saved.add(&self.deps.project_id, &existing.action, save).map_err(SavedError::from)?;
        }
        let primary = {
            let mut pending = self.lock_pending();
            pending.shift_remove(existing.id.0.as_str())
        };
        send_resolution(primary, Resolution::Allow);
        if save.is_none() {
            return Ok(());
        }
        let remaining = {
            let pending = self.lock_pending();
            pending
                .iter()
                .map(|(id, entry)| (id.clone(), entry.request.clone(), entry.agent.clone()))
                .collect::<Vec<_>>()
        };
        for (id, request, agent) in remaining {
            let input = AssertInput {
                id: Some(request.id.clone()),
                session_id: request.session_id.clone(),
                agent,
                action: request.action.clone(),
                resources: request.resources.clone(),
                save: request.save.clone(),
                metadata: request.metadata.clone(),
                source: request.source.clone(),
                unattended: None,
            };
            let result = match self.evaluate_input(&input).await {
                Ok(result) => result,
                Err(PermissionError::SessionNotFound(_)) => continue,
                Err(error) => return Err(error),
            };
            if result.effect != PermissionEffect::Allow {
                continue;
            }
            let entry = {
                let mut pending = self.lock_pending();
                pending.shift_remove(id.as_str())
            };
            let Some(entry) = entry else {
                continue;
            };
            self.deps.events.replied(&entry.request.session_id, &entry.request.id, PermissionReply::Always);
            send_resolution(Some(entry), Resolution::Allow);
        }
        Ok(())
    }
}

impl Drop for NestedGuard {
    fn drop(&mut self) {
        let mut parents = self.parents.lock().unwrap_or_else(|error| error.into_inner());
        let _ = parents.remove(&self.call);
    }
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        let mut pending = self.pending.lock().unwrap_or_else(|error| error.into_inner());
        let _ = pending.shift_remove(self.id.as_str());
    }
}

fn request_id(id: Option<PermissionID>) -> PermissionID {
    id.unwrap_or_else(|| PermissionID(format!("per_{}", opencode_storage::id::ascending())))
}

fn missing_agent_rules() -> Vec<PermissionRule> {
    vec![PermissionRule { action: "*".to_string(), resource: "*".to_string(), effect: PermissionEffect::Deny }]
}

fn denied(input: &AssertInput, rules: &[PermissionRule]) -> bool {
    input.resources.iter().any(|resource| evaluate(&input.action, resource, &[rules]).effect == PermissionEffect::Deny)
}

fn relevant(input: &AssertInput, rules: &[PermissionRule]) -> Vec<PermissionRule> {
    rules
        .iter()
        .filter(|rule| opencode_config::wildcard::matches(&input.action, &rule.action, false))
        .cloned()
        .collect()
}

fn blocked(input: &AssertInput, result: Evaluation) -> PermissionError {
    PermissionError::Blocked {
        rules: relevant(input, result.rules.as_slice()),
        permission: input.action.clone(),
        resources: input.resources.clone(),
        reason: result.message,
    }
}

fn message_id(source: Option<&PermissionSource>) -> Option<&str> {
    source.map(|source| source.message_id.as_str())
}

fn send_resolution(pending: Option<Pending>, resolution: Resolution) {
    let Some(pending) = pending else {
        return;
    };
    let Some(sender) = pending.deferred else {
        return;
    };
    let _ = sender.send(resolution);
}
