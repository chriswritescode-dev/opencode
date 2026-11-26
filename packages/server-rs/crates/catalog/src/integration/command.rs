use std::process::Stdio;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use opencode_contract::generated::{
    CredentialKey, CredentialKeyType, CredentialValue, IntegrationAttemptID, IntegrationID, IntegrationMethod,
    IntegrationMethodID,
};
use opencode_storage::id;
use opencode_util::time::now_ms;
use tokio::io::AsyncReadExt;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::integration::attempt::AttemptTime;
use crate::integration::connect::create_credential;
use crate::integration::{AuthorizationError, IntegrationService};

const ATTEMPT_LIFETIME_MS: i64 = 10 * 60 * 1000;
const TERMINAL_RETENTION: Duration = Duration::from_secs(60);
const MAX_STDERR_BYTES: usize = 64 * 1024;
const MAX_STDOUT_BYTES: usize = 1024 * 1024;
const PIPE_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq)]
pub struct CommandAttempt {
    pub attempt_id: IntegrationAttemptID,
    pub time: AttemptTime,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CommandAttemptStatus {
    Pending { message: Option<String>, time: AttemptTime },
    Complete { time: AttemptTime },
    Failed { message: String, time: AttemptTime },
    Expired { time: AttemptTime },
}

#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error(transparent)]
    Authorization(#[from] AuthorizationError),
    #[error("Command method not found: {}/{}", integration_id.0, method_id.0)]
    MethodNotFound { integration_id: IntegrationID, method_id: IntegrationMethodID },
    #[error("Integration attempt not found: {}/{}", integration_id.0, attempt_id.0)]
    AttemptNotFound { integration_id: IntegrationID, attempt_id: IntegrationAttemptID },
}

pub struct CommandConnectInput {
    pub integration_id: IntegrationID,
    pub method_id: IntegrationMethodID,
    pub label: Option<String>,
}

pub struct CommandService<'a> {
    pub(crate) service: &'a Arc<IntegrationService>,
}

pub(crate) enum CommandEntry {
    Pending(PendingCommand),
    Terminal(TerminalCommand),
}

pub(crate) struct PendingCommand {
    integration_id: IntegrationID,
    label: Option<String>,
    message: String,
    persisting: bool,
    time: AttemptTime,
    expires_at: Instant,
    scope: CancellationToken,
}

pub(crate) struct TerminalCommand {
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
    label: Option<String>,
    time: AttemptTime,
    scope: CancellationToken,
}

enum Claim {
    Success(Claimed),
    Failed(CancellationToken),
    None,
}

impl CommandService<'_> {
    pub async fn connect(&self, input: CommandConnectInput) -> Result<CommandAttempt, CommandError> {
        let command = {
            let registry = self.service.state.get().await.map_err(AuthorizationError::new)?;
            registry.entries.get(&input.integration_id.0).and_then(|entry| {
                entry.info.methods.iter().find_map(|method| match method {
                    IntegrationMethod::Command(method) if method.id == input.method_id => Some(method.command.clone()),
                    _ => None,
                })
            })
        };
        let Some(command) = command else {
            return Err(CommandError::MethodNotFound {
                integration_id: input.integration_id,
                method_id: input.method_id,
            });
        };
        if command.first().is_none_or(|program| program.is_empty()) {
            return Err(CommandError::MethodNotFound {
                integration_id: input.integration_id,
                method_id: input.method_id,
            });
        }
        let created = now_ms();
        let time = AttemptTime { created, expires: created + ATTEMPT_LIFETIME_MS };
        let attempt_id = IntegrationAttemptID(format!("con_{}", id::ascending()));
        let scope = CancellationToken::new();
        {
            let mut attempts = self.service.command_attempts.lock().expect("lock integration attempts");
            attempts.insert(
                attempt_id.0.clone(),
                CommandEntry::Pending(PendingCommand {
                    integration_id: input.integration_id,
                    label: input.label,
                    message: String::new(),
                    persisting: false,
                    time,
                    expires_at: Instant::now() + Duration::from_millis(ATTEMPT_LIFETIME_MS as u64),
                    scope: scope.clone(),
                }),
            );
        }
        let service = Arc::downgrade(self.service);
        let task_attempt_id = attempt_id.clone();
        let task_scope = scope.clone();
        tokio::spawn(async move {
            run(service, task_attempt_id, command, task_scope).await;
        });
        Ok(CommandAttempt { attempt_id, time })
    }

    pub async fn status(
        &self,
        integration_id: &IntegrationID,
        attempt_id: &IntegrationAttemptID,
    ) -> Result<CommandAttemptStatus, CommandError> {
        let attempts = self.service.command_attempts.lock().expect("lock integration attempts");
        let Some(entry) = attempts.get(&attempt_id.0) else {
            return Err(CommandError::AttemptNotFound {
                integration_id: integration_id.clone(),
                attempt_id: attempt_id.clone(),
            });
        };
        match entry {
            CommandEntry::Pending(pending) => {
                if pending.integration_id != *integration_id {
                    return Err(CommandError::AttemptNotFound {
                        integration_id: integration_id.clone(),
                        attempt_id: attempt_id.clone(),
                    });
                }
                Ok(CommandAttemptStatus::Pending {
                    message: (!pending.message.is_empty()).then(|| pending.message.clone()),
                    time: pending.time,
                })
            }
            CommandEntry::Terminal(terminal) => {
                if terminal.integration_id != *integration_id {
                    return Err(CommandError::AttemptNotFound {
                        integration_id: integration_id.clone(),
                        attempt_id: attempt_id.clone(),
                    });
                }
                Ok(match terminal.kind {
                    TerminalKind::Complete => CommandAttemptStatus::Complete { time: terminal.time },
                    TerminalKind::Failed => CommandAttemptStatus::Failed {
                        message: terminal.message.clone().unwrap_or_else(|| "Authentication failed".to_owned()),
                        time: terminal.time,
                    },
                    TerminalKind::Expired => CommandAttemptStatus::Expired { time: terminal.time },
                })
            }
        }
    }

    pub async fn cancel(&self, integration_id: &IntegrationID, attempt_id: &IntegrationAttemptID) {
        let pending = {
            let mut attempts = self.service.command_attempts.lock().expect("lock integration attempts");
            let removable = matches!(
                attempts.get(&attempt_id.0),
                Some(CommandEntry::Pending(pending))
                    if pending.integration_id == *integration_id && !pending.persisting
            );
            if !removable {
                return;
            }
            let Some(CommandEntry::Pending(pending)) = attempts.remove(&attempt_id.0) else {
                return;
            };
            pending
        };
        pending.scope.cancel();
    }
}

async fn run(
    service: Weak<IntegrationService>,
    attempt_id: IntegrationAttemptID,
    command: Vec<String>,
    scope: CancellationToken,
) {
    let Some(outcome) = execute(&service, &attempt_id, &command, &scope).await else {
        return;
    };
    let Some(service) = service.upgrade() else {
        return;
    };
    settle_command(&service, &attempt_id, outcome).await;
}

async fn execute(
    service: &Weak<IntegrationService>,
    attempt_id: &IntegrationAttemptID,
    command: &[String],
    scope: &CancellationToken,
) -> Option<Result<String, AuthorizationError>> {
    let command_env = {
        let service = service.upgrade()?;
        service.command_env.clone()
    };
    let mut builder = tokio::process::Command::new(&command[0]);
    builder
        .args(&command[1..])
        .env_clear()
        .envs(command_env.iter().cloned())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = match builder.spawn() {
        Ok(child) => child,
        Err(error) => return Some(Err(AuthorizationError::new(error))),
    };
    let stderr = child.stderr.take().expect("the command stderr is piped");
    let stdout = child.stdout.take().expect("the command stdout is piped");
    let stderr_service = service.clone();
    let stderr_attempt = attempt_id.clone();
    let mut stderr_task = tokio::spawn(async move {
        let mut reader = stderr;
        let mut buffer = [0u8; 4096];
        let mut pending_bytes = Vec::new();
        while let Ok(read) = reader.read(&mut buffer).await {
            if read == 0 {
                break;
            }
            pending_bytes.extend_from_slice(&buffer[..read]);
            let chunk = decode_stderr(&mut pending_bytes);
            if chunk.is_empty() {
                continue;
            }
            if let Some(service) = stderr_service.upgrade()
                && let Ok(mut attempts) = service.command_attempts.lock()
                && let Some(CommandEntry::Pending(pending)) = attempts.get_mut(&stderr_attempt.0)
            {
                append_stderr(&mut pending.message, &chunk);
            }
        }
        let tail = String::from_utf8_lossy(&pending_bytes).into_owned();
        if !tail.is_empty()
            && let Some(service) = stderr_service.upgrade()
            && let Ok(mut attempts) = service.command_attempts.lock()
            && let Some(CommandEntry::Pending(pending)) = attempts.get_mut(&stderr_attempt.0)
        {
            append_stderr(&mut pending.message, &tail);
        }
    });
    let stdout_bytes = Arc::new(Mutex::new(Vec::new()));
    let mut stdout_task = tokio::spawn({
        let stdout_bytes = stdout_bytes.clone();
        async move {
            let mut reader = stdout;
            let mut buffer = [0u8; 4096];
            while let Ok(read) = reader.read(&mut buffer).await {
                if read == 0 {
                    break;
                }
                let mut collected = stdout_bytes.lock().expect("lock command stdout");
                let capacity = MAX_STDOUT_BYTES.saturating_sub(collected.len());
                collected.extend_from_slice(&buffer[..read.min(capacity)]);
            }
        }
    });
    let status = tokio::select! {
        _ = scope.cancelled() => {
            let _ = child.kill().await;
            stderr_task.abort();
            stdout_task.abort();
            return None;
        }
        status = child.wait() => status,
    };
    let status = match status {
        Ok(status) => status,
        Err(error) => {
            stderr_task.abort();
            stdout_task.abort();
            return Some(Err(AuthorizationError::new(error)));
        }
    };
    let drained = tokio::select! {
        _ = scope.cancelled() => None,
        drained = tokio::time::timeout(PIPE_DRAIN_TIMEOUT, async {
            ((&mut stderr_task).await, (&mut stdout_task).await)
        }) => Some(drained),
    };
    let Some(drained) = drained else {
        stderr_task.abort();
        stdout_task.abort();
        return None;
    };
    if drained.is_err() {
        stderr_task.abort();
        stdout_task.abort();
    }
    let stdout = stdout_bytes.lock().expect("lock command stdout").clone();
    if !status.success() {
        let message = service.upgrade().and_then(|service| {
            let attempts = service.command_attempts.lock().ok()?;
            match attempts.get(&attempt_id.0)? {
                CommandEntry::Pending(pending) => Some(pending.message.clone()),
                CommandEntry::Terminal(_) => None,
            }
        });
        let stderr_message = message.unwrap_or_default();
        let stderr_message = stderr_message.trim();
        let message = if stderr_message.is_empty() {
            format!("Authentication command exited {}", status.code().unwrap_or(-1))
        } else {
            stderr_message.to_owned()
        };
        return Some(Err(AuthorizationError::new(std::io::Error::other(message))));
    }
    let stdout = String::from_utf8_lossy(&stdout).trim().to_owned();
    if stdout.is_empty() {
        return Some(Err(AuthorizationError::new(std::io::Error::other(
            "Authentication command returned no credential",
        ))));
    }
    Some(Ok(stdout))
}

fn decode_stderr(pending: &mut Vec<u8>) -> String {
    let mut decoded = String::new();
    loop {
        match std::str::from_utf8(pending) {
            Ok(text) => {
                decoded.push_str(text);
                pending.clear();
                break;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                if valid > 0 {
                    decoded.push_str(std::str::from_utf8(&pending[..valid]).expect("valid utf-8 prefix"));
                }
                match error.error_len() {
                    Some(len) => {
                        decoded.push('\u{FFFD}');
                        pending.drain(..valid + len);
                    }
                    None => {
                        pending.drain(..valid);
                        break;
                    }
                }
            }
        }
    }
    decoded
}

fn append_stderr(target: &mut String, chunk: &str) {
    target.push_str(chunk);
    if target.len() <= MAX_STDERR_BYTES {
        return;
    }
    let mut boundary = target.len() - MAX_STDERR_BYTES;
    while !target.is_char_boundary(boundary) {
        boundary += 1;
    }
    target.drain(..boundary);
}

async fn settle_command(
    service: &Arc<IntegrationService>,
    attempt_id: &IntegrationAttemptID,
    outcome: Result<String, AuthorizationError>,
) {
    match claim(service, attempt_id, &outcome) {
        Claim::None => {}
        Claim::Failed(scope) => scope.cancel(),
        Claim::Success(claimed) => {
            let Ok(value) = outcome else {
                return;
            };
            let value =
                CredentialValue::Key(CredentialKey { r#type: CredentialKeyType::Key, key: value, metadata: None });
            let persistence = create_credential(service, &claimed.integration_id, claimed.label.clone(), value).await;
            record(service, attempt_id, &claimed, persistence);
            claimed.scope.cancel();
        }
    }
}

fn claim(
    service: &IntegrationService,
    attempt_id: &IntegrationAttemptID,
    outcome: &Result<String, AuthorizationError>,
) -> Claim {
    let mut attempts = service.command_attempts.lock().expect("lock integration attempts");
    let Some(entry) = attempts.get(&attempt_id.0) else {
        return Claim::None;
    };
    let CommandEntry::Pending(pending) = entry else {
        return Claim::None;
    };
    let integration_id = pending.integration_id.clone();
    let label = pending.label.clone();
    let time = pending.time;
    let scope = pending.scope.clone();
    let persisting = pending.persisting;
    if persisting {
        return Claim::None;
    }
    if outcome.is_ok() {
        if let Some(CommandEntry::Pending(pending)) = attempts.get_mut(&attempt_id.0) {
            pending.persisting = true;
        }
        return Claim::Success(Claimed { integration_id, label, time, scope });
    }
    let message = outcome.as_ref().err().map(AuthorizationError::message);
    attempts.insert(
        attempt_id.0.clone(),
        CommandEntry::Terminal(TerminalCommand {
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
    let terminal = TerminalCommand {
        integration_id: claimed.integration_id.clone(),
        message,
        kind,
        time: claimed.time,
        remove_at: Instant::now() + TERMINAL_RETENTION,
    };
    service
        .command_attempts
        .lock()
        .expect("lock integration attempts")
        .insert(attempt_id.0.clone(), CommandEntry::Terminal(terminal));
}

pub(crate) fn scrub(service: &IntegrationService) {
    let now = Instant::now();
    let mut expired = Vec::new();
    {
        let mut attempts = service.command_attempts.lock().expect("lock integration attempts");
        let ids: Vec<String> = attempts
            .iter()
            .filter_map(|(attempt_id, entry)| match entry {
                CommandEntry::Pending(pending) if !pending.persisting && pending.expires_at <= now => {
                    Some(attempt_id.clone())
                }
                CommandEntry::Terminal(terminal) if terminal.remove_at <= now => Some(attempt_id.clone()),
                _ => None,
            })
            .collect();
        for attempt_id in ids {
            let Some(entry) = attempts.remove(&attempt_id) else {
                continue;
            };
            let CommandEntry::Pending(pending) = entry else {
                continue;
            };
            let terminal = TerminalCommand {
                integration_id: pending.integration_id,
                message: None,
                kind: TerminalKind::Expired,
                time: pending.time,
                remove_at: now + TERMINAL_RETENTION,
            };
            attempts.insert(attempt_id, CommandEntry::Terminal(terminal));
            expired.push(pending.scope);
        }
    }
    for scope in expired {
        scope.cancel();
    }
}

pub(crate) fn shutdown(service: &IntegrationService) {
    let mut scopes = Vec::new();
    {
        let attempts = service.command_attempts.lock().expect("lock integration attempts");
        for entry in attempts.values() {
            if let CommandEntry::Pending(pending) = entry {
                scopes.push(pending.scope.clone());
            }
        }
    }
    for scope in scopes {
        scope.cancel();
    }
}
