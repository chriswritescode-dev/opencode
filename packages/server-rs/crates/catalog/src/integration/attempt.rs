use std::sync::Arc;
use std::time::Duration;

use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    CredentialOAuth, CredentialValue, FormAnswer, IntegrationAttemptID, IntegrationID, IntegrationMethodID,
};
use opencode_storage::id;
use opencode_util::time::now_ms;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::integration::connect::{answer_error, create_credential};
use crate::integration::{AuthorizationError, CodeCallback, IntegrationService, OAuthMode, SubmitCallback};

const ATTEMPT_LIFETIME_MS: i64 = 10 * 60 * 1000;
const TERMINAL_RETENTION: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttemptMode {
    Auto,
    Code,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttemptTime {
    pub created: i64,
    pub expires: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Attempt {
    pub attempt_id: IntegrationAttemptID,
    pub url: String,
    pub instructions: String,
    pub mode: AttemptMode,
    pub paste: bool,
    pub time: AttemptTime,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AttemptStatus {
    Pending { time: AttemptTime },
    Complete { time: AttemptTime },
    Failed { message: String, time: AttemptTime },
    Expired { time: AttemptTime },
}

#[derive(Debug, thiserror::Error)]
pub enum AttemptError {
    #[error(transparent)]
    Authorization(#[from] AuthorizationError),
    #[error("OAuth method not found: {}/{}", integration_id.0, method_id.0)]
    MethodNotFound { integration_id: IntegrationID, method_id: IntegrationMethodID },
    #[error("Integration attempt not found: {}/{}", integration_id.0, attempt_id.0)]
    AttemptNotFound { integration_id: IntegrationID, attempt_id: IntegrationAttemptID },
    #[error("Authorization code required for attempt: {}", attempt_id.0)]
    CodeRequired { attempt_id: IntegrationAttemptID },
}

pub struct OAuthConnectInput {
    pub integration_id: IntegrationID,
    pub method_id: IntegrationMethodID,
    pub answer: Option<FormAnswer>,
    pub label: Option<String>,
}

pub struct OAuthService<'a> {
    pub(crate) service: &'a Arc<IntegrationService>,
}

pub(crate) enum OAuthEntry {
    Pending(Pending),
    Terminal(Terminal),
}

pub(crate) struct Pending {
    integration_id: IntegrationID,
    method_id: IntegrationMethodID,
    label: Option<String>,
    completing: bool,
    persisting: bool,
    mode: PendingMode,
    time: AttemptTime,
    expires_at: Instant,
    scope: CancellationToken,
}

enum PendingMode {
    Auto { submit: Option<Arc<SubmitCallback>> },
    Code { callback: Arc<CodeCallback> },
}

pub(crate) struct Terminal {
    integration_id: IntegrationID,
    message: Option<String>,
    kind: TerminalKind,
    time: AttemptTime,
    remove_at: Instant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TerminalKind {
    Complete,
    Failed,
    Expired,
}

struct Claimed {
    integration_id: IntegrationID,
    method_id: IntegrationMethodID,
    label: Option<String>,
    time: AttemptTime,
    scope: CancellationToken,
}

enum Claim {
    Success(Claimed),
    Failed(CancellationToken),
    None,
}

impl OAuthService<'_> {
    pub async fn connect(&self, input: OAuthConnectInput) -> Result<Attempt, AttemptError> {
        let implementation = {
            let registry = self.service.state.get().await.map_err(AuthorizationError::new)?;
            registry
                .entries
                .get(&input.integration_id.0)
                .and_then(|entry| entry.implementations.get(&input.method_id.0))
                .cloned()
        };
        let Some(implementation) = implementation else {
            return Err(AttemptError::MethodNotFound {
                integration_id: input.integration_id,
                method_id: input.method_id,
            });
        };
        let answer = input.answer.unwrap_or(JsonMap(Vec::new()));
        if let Some(invalid) = answer_error(implementation.method().form.as_ref(), &answer, None) {
            return Err(AuthorizationError::new(std::io::Error::other(invalid)).into());
        }
        let authorization = implementation.authorize(answer).await?;
        let url = authorization.url;
        let instructions = authorization.instructions;
        let provider_expires_at = authorization.expires_at;
        let mode = authorization.mode;
        let created = now_ms();
        let time = AttemptTime { created, expires: provider_expires_at.unwrap_or(created + ATTEMPT_LIFETIME_MS) };
        let expires_at = Instant::now()
            + match provider_expires_at {
                Some(expires) => Duration::from_millis((expires - created).max(0) as u64),
                None => Duration::from_millis(ATTEMPT_LIFETIME_MS as u64),
            };
        let (attempt_mode, paste, pending_mode, auto) = match mode {
            OAuthMode::Auto { callback, submit } => {
                let submit = submit.map(Arc::new);
                let paste = submit.is_some();
                (AttemptMode::Auto, paste, PendingMode::Auto { submit }, Some(callback))
            }
            OAuthMode::Code { callback } => {
                (AttemptMode::Code, false, PendingMode::Code { callback: Arc::new(callback) }, None)
            }
        };
        let attempt_id = IntegrationAttemptID(format!("con_{}", id::ascending()));
        let scope = CancellationToken::new();
        {
            let mut attempts = self.service.attempts.lock().expect("lock integration attempts");
            attempts.insert(
                attempt_id.0.clone(),
                OAuthEntry::Pending(Pending {
                    integration_id: input.integration_id,
                    method_id: input.method_id,
                    label: input.label,
                    completing: attempt_mode == AttemptMode::Auto,
                    persisting: false,
                    mode: pending_mode,
                    time,
                    expires_at,
                    scope: scope.clone(),
                }),
            );
        }
        if let Some(callback) = auto {
            let service = Arc::downgrade(self.service);
            let attempt_id = attempt_id.clone();
            let scope = scope.clone();
            tokio::spawn(async move {
                let outcome = tokio::select! {
                    _ = scope.cancelled() => return,
                    outcome = callback() => outcome,
                };
                let Some(service) = service.upgrade() else {
                    return;
                };
                let _ = settle(&service, &attempt_id, outcome).await;
            });
        }
        Ok(Attempt { attempt_id, url, instructions, mode: attempt_mode, paste, time })
    }

    pub async fn status(
        &self,
        integration_id: &IntegrationID,
        attempt_id: &IntegrationAttemptID,
    ) -> Result<AttemptStatus, AttemptError> {
        let attempts = self.service.attempts.lock().expect("lock integration attempts");
        let Some(entry) = attempts.get(&attempt_id.0) else {
            return Err(AttemptError::AttemptNotFound {
                integration_id: integration_id.clone(),
                attempt_id: attempt_id.clone(),
            });
        };
        match entry {
            OAuthEntry::Pending(pending) => {
                if pending.integration_id != *integration_id {
                    return Err(AttemptError::AttemptNotFound {
                        integration_id: integration_id.clone(),
                        attempt_id: attempt_id.clone(),
                    });
                }
                Ok(AttemptStatus::Pending { time: pending.time })
            }
            OAuthEntry::Terminal(terminal) => {
                if terminal.integration_id != *integration_id {
                    return Err(AttemptError::AttemptNotFound {
                        integration_id: integration_id.clone(),
                        attempt_id: attempt_id.clone(),
                    });
                }
                Ok(match terminal.kind {
                    TerminalKind::Complete => AttemptStatus::Complete { time: terminal.time },
                    TerminalKind::Failed => AttemptStatus::Failed {
                        message: terminal.message.clone().unwrap_or_else(|| "Authorization failed".to_owned()),
                        time: terminal.time,
                    },
                    TerminalKind::Expired => AttemptStatus::Expired { time: terminal.time },
                })
            }
        }
    }

    pub async fn complete(
        &self,
        integration_id: &IntegrationID,
        attempt_id: &IntegrationAttemptID,
        code: Option<&str>,
    ) -> Result<(), AttemptError> {
        let action = {
            let mut attempts = self.service.attempts.lock().expect("lock integration attempts");
            let Some(entry) = attempts.get_mut(&attempt_id.0) else {
                return Err(AttemptError::AttemptNotFound {
                    integration_id: integration_id.clone(),
                    attempt_id: attempt_id.clone(),
                });
            };
            match entry {
                OAuthEntry::Terminal(terminal) => {
                    if terminal.integration_id != *integration_id {
                        return Err(AttemptError::AttemptNotFound {
                            integration_id: integration_id.clone(),
                            attempt_id: attempt_id.clone(),
                        });
                    }
                    return Ok(());
                }
                OAuthEntry::Pending(pending) => {
                    if pending.integration_id != *integration_id {
                        return Err(AttemptError::AttemptNotFound {
                            integration_id: integration_id.clone(),
                            attempt_id: attempt_id.clone(),
                        });
                    }
                    match &pending.mode {
                        PendingMode::Auto { submit } => match (code, submit) {
                            (Some(_), Some(submit)) => Action::Submit(submit.clone()),
                            _ => {
                                pending.completing = true;
                                Action::None
                            }
                        },
                        PendingMode::Code { callback } => match code {
                            Some(_) if !pending.completing => {
                                pending.completing = true;
                                Action::Code(callback.clone(), pending.scope.clone())
                            }
                            Some(_) => Action::None,
                            None => {
                                return Err(AttemptError::CodeRequired { attempt_id: attempt_id.clone() });
                            }
                        },
                    }
                }
            }
        };
        match action {
            Action::None => Ok(()),
            Action::Submit(submit) => {
                (submit.as_ref())(code.unwrap_or_default().to_owned()).await?;
                Ok(())
            }
            Action::Code(callback, scope) => {
                let service = self.service.clone();
                let settled_attempt_id = attempt_id.clone();
                let code = code.unwrap_or_default().to_owned();
                let completion = tokio::spawn(async move {
                    let outcome = tokio::select! {
                        outcome = (callback.as_ref())(code) => outcome,
                        _ = scope.cancelled() => Err(AuthorizationError::new(std::io::Error::other(
                            "the authorization attempt was cancelled",
                        ))),
                    };
                    let error = outcome.as_ref().err().cloned();
                    let persistence = settle(&service, &settled_attempt_id, outcome).await;
                    (error, persistence)
                });
                match completion.await {
                    Ok((_, Ok(false))) => Err(AttemptError::AttemptNotFound {
                        integration_id: integration_id.clone(),
                        attempt_id: attempt_id.clone(),
                    }),
                    Ok((Some(error), _)) => Err(error.into()),
                    Ok((None, Err(error))) => Err(error.into()),
                    Ok((None, Ok(true))) => Ok(()),
                    Err(error) => Err(AuthorizationError::new(std::io::Error::other(error)).into()),
                }
            }
        }
    }

    pub async fn cancel(&self, integration_id: &IntegrationID, attempt_id: &IntegrationAttemptID) {
        let pending = {
            let mut attempts = self.service.attempts.lock().expect("lock integration attempts");
            let removable = matches!(
                attempts.get(&attempt_id.0),
                Some(OAuthEntry::Pending(pending))
                    if pending.integration_id == *integration_id && !pending.persisting
            );
            if !removable {
                return;
            }
            let Some(OAuthEntry::Pending(pending)) = attempts.remove(&attempt_id.0) else {
                return;
            };
            pending
        };
        pending.scope.cancel();
        log::info!("integration oauth attempt closed");
    }
}

enum Action {
    None,
    Submit(Arc<SubmitCallback>),
    Code(Arc<CodeCallback>, CancellationToken),
}

async fn settle(
    service: &Arc<IntegrationService>,
    attempt_id: &IntegrationAttemptID,
    outcome: Result<CredentialOAuth, AuthorizationError>,
) -> Result<bool, AuthorizationError> {
    match claim(service, attempt_id, &outcome) {
        Claim::None => Ok(false),
        Claim::Failed(scope) => {
            scope.cancel();
            Ok(true)
        }
        Claim::Success(claimed) => {
            let Ok(value) = outcome else {
                return Ok(true);
            };
            let label = match claimed.label.as_deref() {
                Some(label) => Some(label.to_owned()),
                None => implementation_label(service, &claimed.integration_id, &claimed.method_id, &value).await,
            };
            let persistence =
                create_credential(service, &claimed.integration_id, label, CredentialValue::Oauth(value)).await;
            record(service, attempt_id, &claimed, persistence.clone());
            claimed.scope.cancel();
            persistence.map(|()| true)
        }
    }
}

fn claim(
    service: &IntegrationService,
    attempt_id: &IntegrationAttemptID,
    outcome: &Result<CredentialOAuth, AuthorizationError>,
) -> Claim {
    let mut attempts = service.attempts.lock().expect("lock integration attempts");
    let Some(entry) = attempts.get(&attempt_id.0) else {
        return Claim::None;
    };
    let OAuthEntry::Pending(pending) = entry else {
        return Claim::None;
    };
    let integration_id = pending.integration_id.clone();
    let method_id = pending.method_id.clone();
    let label = pending.label.clone();
    let time = pending.time;
    let scope = pending.scope.clone();
    let persisting = pending.persisting;
    if persisting {
        return Claim::None;
    }
    if outcome.is_ok() {
        if let Some(OAuthEntry::Pending(pending)) = attempts.get_mut(&attempt_id.0) {
            pending.persisting = true;
        }
        return Claim::Success(Claimed { integration_id, method_id, label, time, scope });
    }
    let message = outcome.as_ref().err().map(AuthorizationError::message);
    attempts.insert(
        attempt_id.0.clone(),
        OAuthEntry::Terminal(Terminal {
            integration_id,
            message,
            kind: TerminalKind::Failed,
            time,
            remove_at: Instant::now() + TERMINAL_RETENTION,
        }),
    );
    Claim::Failed(scope)
}

fn record(
    service: &IntegrationService,
    attempt_id: &IntegrationAttemptID,
    claimed: &Claimed,
    persistence: Result<(), AuthorizationError>,
) {
    let (kind, message) = match persistence {
        Ok(()) => (TerminalKind::Complete, None),
        Err(error) => (TerminalKind::Failed, Some(error.message())),
    };
    let terminal = Terminal {
        integration_id: claimed.integration_id.clone(),
        message,
        kind,
        time: claimed.time,
        remove_at: Instant::now() + TERMINAL_RETENTION,
    };
    service
        .attempts
        .lock()
        .expect("lock integration attempts")
        .insert(attempt_id.0.clone(), OAuthEntry::Terminal(terminal));
}

async fn implementation_label(
    service: &Arc<IntegrationService>,
    integration_id: &IntegrationID,
    method_id: &IntegrationMethodID,
    value: &CredentialOAuth,
) -> Option<String> {
    let registry = service.state.get().await.ok()?;
    let implementation = registry.entries.get(&integration_id.0)?.implementations.get(&method_id.0)?.clone();
    let label = implementation.label()?;
    label(value)
}

pub(crate) fn scrub(service: &IntegrationService) {
    let now = Instant::now();
    let mut expired = Vec::new();
    {
        let mut attempts = service.attempts.lock().expect("lock integration attempts");
        let ids: Vec<String> = attempts
            .iter()
            .filter_map(|(attempt_id, entry)| match entry {
                OAuthEntry::Pending(pending) if !pending.persisting && pending.expires_at <= now => {
                    Some(attempt_id.clone())
                }
                OAuthEntry::Terminal(terminal) if terminal.remove_at <= now => Some(attempt_id.clone()),
                _ => None,
            })
            .collect();
        for attempt_id in ids {
            let Some(entry) = attempts.remove(&attempt_id) else {
                continue;
            };
            let OAuthEntry::Pending(pending) = entry else {
                continue;
            };
            let terminal = Terminal {
                integration_id: pending.integration_id,
                message: None,
                kind: TerminalKind::Expired,
                time: pending.time,
                remove_at: now + TERMINAL_RETENTION,
            };
            attempts.insert(attempt_id, OAuthEntry::Terminal(terminal));
            expired.push(pending.scope);
            log::info!("integration oauth attempt closed");
        }
    }
    for scope in expired {
        scope.cancel();
    }
}

pub(crate) fn shutdown(service: &IntegrationService) {
    let mut scopes = Vec::new();
    {
        let attempts = service.attempts.lock().expect("lock integration attempts");
        for entry in attempts.values() {
            if let OAuthEntry::Pending(pending) = entry {
                scopes.push(pending.scope.clone());
            }
        }
    }
    for scope in scopes {
        scope.cancel();
    }
}
