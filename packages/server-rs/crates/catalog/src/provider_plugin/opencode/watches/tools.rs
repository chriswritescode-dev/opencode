use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures_util::FutureExt;
use opencode_config::seams::KvStore;
use opencode_contract::generated::SessionID;
use serde::Deserialize;
use serde_json::{Value, json};

use super::wake;
use crate::provider_plugin::hooks::ToolExecuteAfter;
use crate::provider_plugin::http::{self, BufferedResponse, HttpError};
use crate::provider_plugin::opencode::handle::ConsoleHandle;
use crate::provider_plugin::{BoxFuture, SessionAccess, SyntheticError, SyntheticInput};

const MAX_DESCRIPTION: usize = 300;

const MAX_MINUTES: u64 = 14 * 24 * 60;

const UNATTENDED_PREFIX: &str = "watch-unattended:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolFailure {
    pub message: String,
    pub status: Option<u16>,
}

impl ToolFailure {
    fn message(message: impl Into<String>) -> Self {
        Self { message: message.into(), status: None }
    }
}

#[derive(Clone)]
pub struct WatchTools {
    console: Arc<Mutex<wake::Console>>,
    kv: Arc<dyn KvStore>,
    sessions: Arc<dyn SessionAccess>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct Watch {
    id: String,
    #[serde(rename = "sessionID")]
    session_id: String,
    #[serde(rename = "routingKey")]
    routing_key: String,
    #[serde(rename = "expiresAt")]
    expires_at: String,
    #[serde(rename = "serverSlug")]
    server_slug: Option<String>,
    #[serde(rename = "eventType")]
    event_type: Option<String>,
    arguments: Option<Value>,
}

#[derive(Deserialize)]
struct WatchList {
    watches: Vec<Watch>,
}

#[derive(Deserialize)]
struct EventType {
    server: String,
    name: String,
    description: Option<String>,
    #[serde(rename = "inputSchema")]
    input_schema: Value,
}

#[derive(Deserialize)]
struct EventTypeList {
    types: Vec<EventType>,
}

#[derive(Deserialize)]
struct Problem {
    message: Option<String>,
    limit: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ShellOutput {
    #[serde(rename = "exitCode")]
    pub exit_code: Option<i64>,
    pub output: String,
}

impl WatchTools {
    pub(crate) fn new(
        console: Arc<Mutex<wake::Console>>,
        kv: Arc<dyn KvStore>,
        sessions: Arc<dyn SessionAccess>,
    ) -> Self {
        Self { console, kv, sessions }
    }

    pub async fn watch(&self, session_id: &str, input: Value) -> Result<String, ToolFailure> {
        let target = input.get("target").filter(|value| !value.is_null());
        let server = input.get("server").and_then(Value::as_str);
        let event = input.get("event").and_then(Value::as_str);
        let watched = match (target, server, event) {
            (Some(target), None, None) => json!({ "target": target }),
            (None, Some(server), Some(event)) => json!({
                "server": server,
                "event": event,
                "arguments": input.get("arguments").cloned().unwrap_or_else(|| json!({})),
            }),
            _ => {
                return Err(ToolFailure::message(
                    "watch takes a target, or a server and event with the event's arguments, not both",
                ));
            }
        };
        if let Some(minutes) = input.get("minutes")
            && !minutes.as_u64().is_some_and(|minutes| (1..=MAX_MINUTES).contains(&minutes))
        {
            return Err(ToolFailure::message(format!("minutes must be between 1 and {MAX_MINUTES}")));
        }
        let mut body = watched;
        body["sessionID"] = json!(session_id);
        if let Some(minutes) = input.get("minutes") {
            body["minutes"] = minutes.clone();
        }
        let watch = self.create(&body).await?;
        if input.get("unattended").and_then(Value::as_bool) == Some(true) {
            self.kv.set(&format!("{UNATTENDED_PREFIX}{}", watch.id), json!(true)).await;
        }
        Ok(format!(
            "Watching {} until {} ({}). This session wakes when it changes.",
            label(&watch),
            watch.expires_at,
            watch.id,
        ))
    }

    pub async fn unwatch(&self, session_id: &str, input: Value) -> Result<String, ToolFailure> {
        let Some(id) = input.get("id").and_then(Value::as_str) else {
            return Err(ToolFailure::message("unwatch takes the ID of a watch"));
        };
        let owned = self.list().await?.iter().any(|watch| watch.id == id && watch.session_id == session_id);
        if !owned {
            return Err(ToolFailure::message(format!("This session has no watch {id}")));
        }
        self.call("DELETE", &format!("/api/event-watches/{}", encode_component(id)), None).await?;
        Ok(format!("Stopped watch {id}."))
    }

    pub async fn watches(&self, session_id: &str, _input: Value) -> Result<String, ToolFailure> {
        let watches: Vec<Watch> =
            self.list().await?.into_iter().filter(|watch| watch.session_id == session_id).collect();
        if watches.is_empty() {
            return Ok("This session watches nothing.".to_owned());
        }
        Ok(watches
            .iter()
            .map(|watch| format!("{}: {} until {}", watch.id, label(watch), watch.expires_at))
            .collect::<Vec<_>>()
            .join("\n"))
    }

    pub async fn events(&self, _session_id: &str, _input: Value) -> Result<String, ToolFailure> {
        let response = self.call("GET", "/api/event-watches/types", None).await?;
        let list: EventTypeList = serde_json::from_slice(&response.body).map_err(|error| {
            ToolFailure::message(format!("OpenCode Console sent an unreadable event list: {error}"))
        })?;
        if list.types.is_empty() {
            return Ok("No event types to watch: connect a server that offers them in Extensions.".to_owned());
        }
        Ok(list
            .types
            .iter()
            .map(|event_type| {
                let description = event_type.description.as_deref().map(one_line).filter(|value| !value.is_empty());
                match description {
                    Some(description) => format!(
                        "{} {}: {description} Arguments: {}",
                        event_type.server,
                        event_type.name,
                        serde_json::to_string(&event_type.input_schema).expect("an event type schema serializes"),
                    ),
                    None => format!(
                        "{} {} Arguments: {}",
                        event_type.server,
                        event_type.name,
                        serde_json::to_string(&event_type.input_schema).expect("an event type schema serializes"),
                    ),
                }
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }

    async fn create(&self, body: &Value) -> Result<Watch, ToolFailure> {
        let response = self.call("POST", "/api/event-watches", Some(body)).await?;
        serde_json::from_slice(&response.body)
            .map_err(|error| ToolFailure::message(format!("OpenCode Console sent an unreadable watch: {error}")))
    }

    async fn list(&self) -> Result<Vec<Watch>, ToolFailure> {
        let response = self.call("GET", "/api/event-watches", None).await?;
        let list: WatchList = serde_json::from_slice(&response.body)
            .map_err(|error| ToolFailure::message(format!("OpenCode Console sent unreadable watches: {error}")))?;
        Ok(list.watches)
    }

    async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<BufferedResponse, ToolFailure> {
        let console = self.console.lock().expect("lock the Console watch connection").clone();
        let mut headers = (console.headers)().await.map_err(ToolFailure::message)?;
        headers.push(("accept".to_owned(), "application/json".to_owned()));
        let url = format!("{}{path}", console.server);
        let request = match body {
            Some(body) => http::json(url, headers, body),
            None => http::PreparedHttp::new(method, url, headers, Bytes::new()),
        };
        match http::send(console.http.as_ref(), request, None).await {
            Ok(response) => Ok(response),
            Err(HttpError::Status(response)) => Err(problem(&response)),
            Err(error) => Err(ToolFailure::message(error.to_string())),
        }
    }
}

fn problem(response: &BufferedResponse) -> ToolFailure {
    let problem: Option<Problem> = serde_json::from_slice(&response.body).ok();
    let message = match problem.as_ref().and_then(|problem| problem.limit) {
        Some(limit) => {
            format!("This member already keeps the most watches Console allows ({limit}). Stop one with unwatch first.")
        }
        None if response.status == 404 => "Event watches are off for this OpenCode Console workspace.".to_owned(),
        None => problem
            .and_then(|problem| problem.message)
            .unwrap_or_else(|| format!("OpenCode Console answered {}", response.status)),
    };
    ToolFailure { message, status: Some(response.status) }
}

fn label(watch: &Watch) -> String {
    match (&watch.server_slug, &watch.event_type) {
        (Some(server), Some(event)) if watch.routing_key.starts_with("mcp:") => {
            let arguments = serde_json::to_string(watch.arguments.as_ref().unwrap_or(&json!({})))
                .expect("watch arguments serialize");
            format!("{server} {event} {arguments}")
        }
        _ => watch.routing_key.clone(),
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(MAX_DESCRIPTION).collect()
}

fn encode_component(value: &str) -> String {
    const COMPONENT: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'_')
        .remove(b'.')
        .remove(b'!')
        .remove(b'~')
        .remove(b'*')
        .remove(b'\'')
        .remove(b'(')
        .remove(b')');
    percent_encoding::utf8_percent_encode(value, COMPONENT).to_string()
}

/// What a finished shell command started that's worth watching: the pull request `gh pr create` printed, or the
/// branch `git push` updated on GitHub. Anything less clear-cut is skipped.
pub fn shell_target(command: &str, shell: &ShellOutput) -> Option<Value> {
    if shell.exit_code != Some(0) {
        return None;
    }
    let created =
        regex_lite::Regex::new(r"(?:^|[\s;&|(])gh\s+pr\s+create\b").expect("the gh pr create pattern compiles");
    if created.is_match(command) {
        let url = regex_lite::Regex::new(r"https://github\.com/[\w.-]+/[\w.-]+/pull/\d+")
            .expect("the pull request URL pattern compiles");
        let mut urls: Vec<&str> = Vec::new();
        for found in url.find_iter(&shell.output) {
            if !urls.contains(&found.as_str()) {
                urls.push(found.as_str());
            }
        }
        return (urls.len() == 1).then(|| json!(urls[0]));
    }
    let pushed =
        regex_lite::Regex::new(r"(?:^|[\s;&|(])git\s+(?:-C\s+\S+\s+)?push\b").expect("the git push pattern compiles");
    if !pushed.is_match(command) {
        return None;
    }
    let remotes = regex_lite::Regex::new(
        r"(?m)^To (?:https://github\.com/|(?:ssh://)?(?:git@)?github\.com[:/])([\w.-]+/[\w.-]+?)(?:\.git)?/?$",
    )
    .expect("the GitHub remote pattern compiles");
    let branches =
        regex_lite::Regex::new(r"(?m)^\s*[+*]?\s*(?:[0-9a-f]+\.\.\.?[0-9a-f]+|\[new branch\])\s+\S+\s+->\s+(\S+)")
            .expect("the pushed branch pattern compiles");
    let remotes: Vec<&str> =
        remotes.captures_iter(&shell.output).filter_map(|captures| captures.get(1).map(|repo| repo.as_str())).collect();
    let branches: Vec<&str> = branches
        .captures_iter(&shell.output)
        .filter_map(|captures| captures.get(1).map(|branch| branch.as_str()))
        .collect();
    let (repo, branch) = match (remotes.as_slice(), branches.as_slice()) {
        ([repo], [branch]) => (*repo, *branch),
        _ => return None,
    };
    Some(json!({ "_tag": "GitHubBranch", "repo": repo.to_lowercase(), "branch": branch }))
}

pub(crate) fn shell_hook(
    console: Arc<ConsoleHandle>,
) -> impl Fn(ToolExecuteAfter) -> BoxFuture<ToolExecuteAfter> + Send + Sync + 'static {
    move |event| {
        let console = console.clone();
        async move {
            if event.tool == "shell" {
                watch_shell(&console, &event);
            }
            event
        }
        .boxed()
    }
}

fn watch_shell(console: &Arc<ConsoleHandle>, event: &ToolExecuteAfter) {
    let Some(output) = event.output.as_ref() else {
        return;
    };
    let Some(command) = event.input.get("command").and_then(Value::as_str) else {
        return;
    };
    let Ok(shell) = serde_json::from_value::<ShellOutput>(output.clone()) else {
        return;
    };
    let Some(target) = shell_target(command, &shell) else {
        return;
    };
    let Some(tools) = console.watch_tools() else {
        return;
    };
    let session_id = event.session_id.clone();
    tokio::spawn(async move {
        match tools.watch(&session_id.0, json!({ "target": target })).await {
            Ok(_) => {}
            Err(refusal) if refusal.status == Some(403) => tell_once(&tools, &session_id, &refusal.message).await,
            Err(failure) => log::debug!("skipped watching a shell command's work: {}", failure.message),
        }
    });
}

async fn tell_once(tools: &WatchTools, session_id: &SessionID, message: &str) {
    let id = format!(
        "msg_watch_{}",
        &opencode_util::hash::sha256_hex(format!("{}\n{message}", session_id.0).as_bytes())[..26]
    );
    let delivered = tools
        .sessions
        .synthetic(SyntheticInput {
            session_id: session_id.clone(),
            id,
            text: format!(
                "OpenCode Console didn't watch a pull request or push from this session, so its updates won't wake \
                 the session: {message}"
            ),
            description: String::new(),
            metadata: json!({ "source": "console.watch" }),
            delivery: "steer",
            resume: true,
        })
        .await;
    if let Err(SyntheticError::Unavailable(error)) = delivered {
        log::debug!("failed to tell a session that Console refused a watch: {error}");
    }
}
