use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue};
use tokio_tungstenite::tungstenite::protocol::frame::CloseFrame;

use crate::provider_plugin::BoxFuture;
use crate::provider_plugin::http::{self, HttpError};

pub const LEASE_CLOSE_CODE: u16 = 4401;

pub const OVERSIZED_CLOSE_CODE: u16 = 1009;

pub const ACK_LIMIT: usize = 1000;

const SILENCE: Duration = Duration::from_secs(11 * 60);

const POLL_WAIT: u64 = 25;

const POLL_RETRY_WEBSOCKET: Duration = Duration::from_secs(10 * 60);

const POLL_TIMEOUT: Duration = Duration::from_secs(POLL_WAIT + 10);

const MAX_BACKOFF: Duration = Duration::from_secs(60);

pub(crate) type Headers = Arc<dyn Fn() -> BoxFuture<Result<Vec<(String, String)>, String>> + Send + Sync>;

pub(crate) type Deliver = Arc<dyn Fn(Vec<WatchEvent>) -> BoxFuture<Vec<String>> + Send + Sync>;

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct WatchEvent {
    pub id: String,
    #[serde(rename = "watchID")]
    pub watch_id: String,
    #[serde(rename = "sessionID")]
    pub session_id: String,
    pub source: String,
    pub kind: String,
    pub actor: WatchActor,
    pub title: String,
    pub body: String,
    pub url: String,
    pub status: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct WatchActor {
    pub login: String,
    pub bot: bool,
    #[serde(rename = "self", default)]
    pub itself: Option<bool>,
}

#[derive(Deserialize)]
struct ServerMessage {
    #[serde(rename = "type")]
    kind: String,
    events: Vec<WatchEvent>,
}

#[derive(Deserialize)]
struct Poll {
    cursor: String,
    events: Vec<WatchEvent>,
}

pub(crate) struct WatchTarget {
    pub key: String,
    pub server: String,
    pub headers: Headers,
    pub http: Arc<dyn opencode_ai::HttpSender>,
    pub sessions: Arc<dyn crate::provider_plugin::SessionAccess>,
    pub kv: Arc<dyn opencode_config::seams::KvStore>,
    pub clock: Arc<dyn opencode_ai::clock::Clock>,
}

#[derive(Clone)]
struct Member {
    id: u64,
    server: String,
    headers: Headers,
    http: Arc<dyn opencode_ai::HttpSender>,
    deliver: Deliver,
}

struct HubState {
    members: Vec<Member>,
}

struct Hub {
    state: Arc<Mutex<HubState>>,
    task: JoinHandle<()>,
}

static HUBS: OnceLock<Mutex<HashMap<String, Hub>>> = OnceLock::new();

static NEXT_MEMBER: AtomicU64 = AtomicU64::new(0);

fn hubs() -> &'static Mutex<HashMap<String, Hub>> {
    HUBS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn join(target: WatchTarget, deliver: Deliver) -> u64 {
    let id = NEXT_MEMBER.fetch_add(1, Ordering::Relaxed);
    let member = Member { id, server: target.server, headers: target.headers, http: target.http, deliver };
    let mut hubs = hubs().lock().expect("lock the Console watch hubs");
    if let Some(hub) = hubs.get_mut(&target.key) {
        hub.state.lock().expect("lock the Console watch hub members").members.push(member);
        return id;
    }
    let state = Arc::new(Mutex::new(HubState { members: vec![member] }));
    let task = tokio::spawn(run(state.clone()));
    hubs.insert(target.key, Hub { state, task });
    id
}

pub(crate) fn update(key: &str, id: u64, target: WatchTarget) {
    let hubs = hubs().lock().expect("lock the Console watch hubs");
    let Some(hub) = hubs.get(key) else {
        return;
    };
    let mut state = hub.state.lock().expect("lock the Console watch hub members");
    if let Some(member) = state.members.iter_mut().find(|member| member.id == id) {
        member.server = target.server;
        member.headers = target.headers;
        member.http = target.http;
    }
}

pub(crate) fn leave(key: &str, id: u64) {
    let mut hubs = hubs().lock().expect("lock the Console watch hubs");
    let Some(hub) = hubs.get_mut(key) else {
        return;
    };
    let empty = {
        let mut state = hub.state.lock().expect("lock the Console watch hub members");
        state.members.retain(|member| member.id != id);
        state.members.is_empty()
    };
    if !empty {
        return;
    }
    let hub = hubs.remove(key).expect("the Console watch hub being left");
    hub.task.abort();
}

fn leader(state: &Mutex<HubState>) -> Option<Member> {
    state.lock().expect("lock the Console watch hub members").members.first().cloned()
}

pub(crate) fn is_leader(key: &str, id: u64) -> bool {
    let hubs = hubs().lock().expect("lock the Console watch hubs");
    let Some(hub) = hubs.get(key) else {
        return false;
    };
    let state = hub.state.lock().expect("lock the Console watch hub members");
    state.members.first().is_some_and(|member| member.id == id)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Lease,
    Closed,
    Refused,
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Polled {
    Websocket,
    Completed,
    Failed,
}

async fn run(state: Arc<Mutex<HubState>>) {
    let mut failures = 0u32;
    loop {
        let outcome = stream(&state).await;
        if outcome == Outcome::Lease {
            failures = 0;
            continue;
        }
        if outcome == Outcome::Closed {
            failures = 0;
        }
        if outcome == Outcome::Refused {
            match poll(&state).await {
                Polled::Websocket => {
                    failures = 0;
                    continue;
                }
                Polled::Completed => failures = 0,
                Polled::Failed => {}
            }
        }
        failures = failures.saturating_add(1);
        let backoff = Duration::from_millis(1000u64.saturating_mul(2u64.saturating_pow(failures.saturating_sub(1))))
            .min(MAX_BACKOFF);
        tokio::time::sleep(backoff).await;
    }
}

async fn stream(state: &Mutex<HubState>) -> Outcome {
    let Some(member) = leader(state) else {
        return Outcome::Failed;
    };
    let headers = match (member.headers)().await {
        Ok(headers) => headers,
        Err(_) => return Outcome::Failed,
    };
    let mut request = match socket_url(&member.server).into_client_request() {
        Ok(request) => request,
        Err(_) => return Outcome::Failed,
    };
    for (name, value) in headers {
        let (Ok(name), Ok(value)) = (HeaderName::try_from(name.as_str()), HeaderValue::try_from(value.as_str())) else {
            continue;
        };
        request.headers_mut().insert(name, value);
    }
    let socket = match tokio_tungstenite::connect_async(request).await {
        Ok((socket, _)) => socket,
        Err(tokio_tungstenite::tungstenite::Error::Http(_))
        | Err(tokio_tungstenite::tungstenite::Error::Protocol(
            tokio_tungstenite::tungstenite::error::ProtocolError::HandshakeIncomplete,
        )) => return Outcome::Refused,
        Err(_) => return Outcome::Failed,
    };
    let mut socket = socket;
    loop {
        let message = match tokio::time::timeout(SILENCE, socket.next()).await {
            Err(_) => return Outcome::Closed,
            Ok(None) => return Outcome::Closed,
            Ok(Some(Err(_))) => return Outcome::Closed,
            Ok(Some(Ok(message))) => message,
        };
        let text = match message {
            Message::Text(text) => text.as_str().to_owned(),
            Message::Binary(bytes) => match String::from_utf8(bytes.to_vec()) {
                Ok(text) => text,
                Err(_) => continue,
            },
            Message::Close(frame) => return closed(frame),
            _ => continue,
        };
        let Ok(decoded) = serde_json::from_str::<ServerMessage>(&text) else {
            log::warn!("ignored an event stream message this version can't read");
            continue;
        };
        if decoded.kind != "snapshot" && decoded.kind != "events" {
            log::warn!("ignored an event stream message this version can't read");
            continue;
        }
        let Some(member) = leader(state) else {
            return Outcome::Closed;
        };
        let ids = (member.deliver)(decoded.events).await;
        for chunk in chunks(&ids) {
            let body = json!({ "type": "ack", "ids": chunk });
            let text = serde_json::to_string(&body).expect("a Console watch acknowledgement serializes");
            if socket.send(Message::text(text)).await.is_err() {
                return Outcome::Closed;
            }
        }
    }
}

fn closed(frame: Option<CloseFrame>) -> Outcome {
    let Some(frame) = frame else {
        return Outcome::Closed;
    };
    match u16::from(frame.code) {
        LEASE_CLOSE_CODE => Outcome::Lease,
        OVERSIZED_CLOSE_CODE => {
            log::warn!("Console closed the event stream: a message this client sent was too long");
            Outcome::Closed
        }
        _ => Outcome::Closed,
    }
}

fn socket_url(server: &str) -> String {
    let (scheme, rest) = if let Some(rest) = server.strip_prefix("https") {
        ("wss", rest)
    } else if let Some(rest) = server.strip_prefix("http") {
        ("ws", rest)
    } else {
        return format!("{server}/api/event-watches/stream");
    };
    format!("{scheme}{rest}/api/event-watches/stream")
}

async fn poll(state: &Mutex<HubState>) -> Polled {
    let started = Instant::now();
    let mut cursor: Option<String> = None;
    let mut polled = false;
    loop {
        if polled && started.elapsed() >= POLL_RETRY_WEBSOCKET {
            return Polled::Websocket;
        }
        let Some(member) = leader(state) else {
            return Polled::Failed;
        };
        let url = {
            let mut query = url::form_urlencoded::Serializer::new(String::new());
            query.append_pair("wait", &POLL_WAIT.to_string());
            if let Some(cursor) = &cursor {
                query.append_pair("cursor", cursor);
            }
            format!("{}/api/event-watches/events?{}", member.server, query.finish())
        };
        let page = match call(&member, "GET", url, None).await {
            Ok(response) => serde_json::from_slice::<Poll>(&response.body).ok(),
            Err(_) => None,
        };
        let Some(page) = page else {
            return if polled { Polled::Completed } else { Polled::Failed };
        };
        polled = true;
        let ids = (member.deliver)(page.events).await;
        for chunk in chunks(&ids) {
            let body = json!({ "ids": chunk });
            if call(&member, "POST", format!("{}/api/event-watches/ack", member.server), Some(&body)).await.is_err() {
                return Polled::Completed;
            }
        }
        cursor = Some(page.cursor);
    }
}

async fn call(
    member: &Member,
    method: &str,
    url: String,
    body: Option<&serde_json::Value>,
) -> Result<http::BufferedResponse, HttpError> {
    let mut headers = (member.headers)().await.map_err(HttpError::Transport)?;
    if !headers.iter().any(|(name, _)| name.eq_ignore_ascii_case("accept")) {
        headers.push(("accept".to_owned(), "application/json".to_owned()));
    }
    let request = match (method, body) {
        ("POST", Some(body)) => http::json(url, headers, body),
        _ => http::get(url, headers),
    };
    http::send(&*member.http, request, Some(POLL_TIMEOUT)).await
}

fn chunks(ids: &[String]) -> Vec<&[String]> {
    ids.chunks(ACK_LIMIT).collect()
}
