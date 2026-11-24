#![allow(clippy::type_complexity)]

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::ExecutionContext;
use crate::route::executor::BoxFuture;
use crate::schema::{AiError, AiErrorReason};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Running,
    Completed,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Reason {
    Failed,
    Cancelled,
    Expired,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub id: String,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<Reason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f64>,
}

pub fn is_terminal(status: Status) -> bool {
    status != Status::Running
}

pub struct GenerationRoute<Response> {
    pub status: Arc<dyn Fn() -> BoxFuture<Result<Snapshot, AiError>> + Send + Sync>,
    pub result: Arc<dyn Fn() -> BoxFuture<Result<Response, AiError>> + Send + Sync>,
    pub cancel: Option<Arc<dyn Fn() -> BoxFuture<Result<(), AiError>> + Send + Sync>>,
}

impl<Response> Clone for GenerationRoute<Response> {
    fn clone(&self) -> Self {
        Self { status: self.status.clone(), result: self.result.clone(), cancel: self.cancel.clone() }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Poll {
    pub interval: Option<Duration>,
    pub timeout: Option<Duration>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AwaitOptions {
    pub poll: Option<Poll>,
}

pub struct Generation<Response> {
    pub id: String,
    pub status: Status,
    pub reason: Option<Reason>,
    pub progress: Option<f64>,
    pub route: GenerationRoute<Response>,
    pub token: Value,
}

impl<Response: 'static> Generation<Response> {
    pub fn new(route: GenerationRoute<Response>, token: Value, snapshot: Snapshot) -> Self {
        Self {
            id: snapshot.id,
            status: snapshot.status,
            reason: snapshot.reason,
            progress: snapshot.progress,
            route,
            token,
        }
    }

    pub fn terminal(&self) -> bool {
        is_terminal(self.status)
    }

    pub fn refresh(&self) -> BoxFuture<Result<Self, AiError>> {
        let status = self.route.status.clone();
        let route = self.route.clone();
        let token = self.token.clone();
        Box::pin(async move {
            let snapshot = status.as_ref()().await?;
            Ok(Self::new(route, token, snapshot))
        })
    }

    pub fn result(&self) -> BoxFuture<Result<Response, AiError>> {
        self.route.result.as_ref()()
    }

    pub fn cancel(&self) -> BoxFuture<Result<(), AiError>> {
        match &self.route.cancel {
            Some(cancel) => cancel.as_ref()(),
            None => Box::pin(async { Ok(()) }),
        }
    }

    pub async fn await_result(
        &self,
        context: &ExecutionContext,
        options: Option<AwaitOptions>,
    ) -> Result<Response, AiError> {
        let poll = options.and_then(|options| options.poll).unwrap_or_default();
        let interval = poll.interval.unwrap_or(Duration::from_secs(5));
        let timeout = poll.timeout.unwrap_or(Duration::from_secs(600));
        let timeout_ms = timeout.as_millis().min(u128::from(u64::MAX)) as u64;
        let deadline = context.clock.now_ms().saturating_add(timeout_ms);
        if self.terminal() {
            return self.result().await;
        }
        loop {
            let now = context.clock.now_ms();
            if now >= deadline {
                return Err(timeout_error(&self.id, timeout, timeout_ms));
            }
            let current = self.refresh().await?;
            if current.terminal() {
                return current.result().await;
            }
            let remaining = Duration::from_millis(deadline - now);
            context.sleeper.sleep(interval.min(remaining)).await;
        }
    }
}

fn timeout_error(id: &str, timeout: Duration, timeout_ms: u64) -> AiError {
    AiError {
        reason: AiErrorReason::Timeout {
            message: format!("Generation {id} did not finish within {}", format_duration(timeout)),
            body: None,
            http: None,
            timeout_ms: Some(timeout_ms),
        },
    }
}

fn format_duration(duration: Duration) -> String {
    let millis = duration.as_millis().min(u128::from(u64::MAX)) as u64;
    if millis == 0 {
        return "0ms".to_string();
    }
    if millis.is_multiple_of(60_000) {
        return format!("{}m", millis / 60_000);
    }
    if millis.is_multiple_of(1_000) {
        return format!("{}s", millis / 1_000);
    }
    format!("{millis}ms")
}
