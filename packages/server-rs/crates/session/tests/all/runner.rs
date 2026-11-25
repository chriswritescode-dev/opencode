use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use futures_util::future::BoxFuture;
use opencode_ai::{Env, MapEnv};
use opencode_bus::{Bus, BusOptions, Event, Subscription};
use opencode_catalog::Catalog;
use opencode_config::entry::{Document, Entry, Origin};
use opencode_contract::codec::{decode_json, encode_json};
use opencode_contract::generated::{
    ConfigEntryDocumentInfo, LocationRef, SessionInboxDelivery, SessionMessageIdleOutcome, SessionMessageInfo,
};
use opencode_location::environment::local::LocalEnvironment;
use opencode_location::project::ProjectService;
use opencode_registry::agent::AgentService;
use opencode_session::instructions::BuiltinsInput;
use opencode_session::model_request::AppInfo;
use opencode_session::runner::Runner;
use opencode_session::{
    Execution, MessagesInput, Order, Placed, Placement, ProjectionSettings, PromptInput, SessionError,
    SessionExecution, SessionProjector, SessionStore, Sessions,
};
use opencode_storage::{Database, DatabaseLocation};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::{TempDir, TestAgents, test_agents};

const FIXED_NOW: i64 = 1_800_000_000_000;
const CASSETTE: &str = include_str!(
    "../../../../../ai/test/fixtures/recordings/anthropic-compatible-messages/minimax-m3-anthropic-compatible-text.json"
);

struct Setup {
    _dir: TempDir,
    database: Arc<Database>,
    bus: Arc<Bus>,
    store: SessionStore,
    sessions: Sessions,
}

fn app() -> AppInfo {
    AppInfo { name: "opencode".to_owned(), channel: "local".to_owned(), version: "0.0.0-test".to_owned() }
}

fn builtins_input(work: &str, root: &Path) -> BuiltinsInput {
    BuiltinsInput {
        directory: work.to_owned(),
        workspace_root: work.to_owned(),
        vcs: None,
        tmp: root.join("tmp/opencode").to_string_lossy().into_owned(),
    }
}

fn cassette_body() -> String {
    let cassette: Value = serde_json::from_str(CASSETTE).expect("parse the cassette");
    cassette["interactions"][0]["response"]["body"].as_str().expect("the cassette carries a body").to_owned()
}

fn config_value(port: u16) -> Value {
    json!({
        "model": "anthropic-test/test-model",
        "agents": { "title": { "disabled": true } },
        "providers": {
            "anthropic-test": {
                "name": "Anthropic Test",
                "package": "@ai-sdk/anthropic",
                "settings": { "apiKey": "parity", "baseURL": format!("http://127.0.0.1:{port}") },
                "models": {
                    "test-model": {
                        "name": "Test Model",
                        "capabilities": { "tools": true, "input": ["text"], "output": ["text"] },
                        "cost": { "input": 0, "output": 0 },
                        "limit": { "context": 100000, "output": 10000 }
                    }
                }
            }
        }
    })
}

struct TestPlaced {
    catalog: Catalog,
    agents: TestAgents,
    builtins: BuiltinsInput,
    env: MapEnv,
    closed: AtomicBool,
}

impl Placed for TestPlaced {
    fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    fn agents(&self) -> &AgentService {
        &self.agents.service
    }

    fn builtins(&self) -> &BuiltinsInput {
        &self.builtins
    }

    fn env(&self) -> &dyn Env {
        &self.env
    }

    fn location(&self) -> Option<&LocationRef> {
        None
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

struct TestPlacement {
    placements: Vec<Arc<TestPlaced>>,
    calls: AtomicUsize,
}

impl Placement for TestPlacement {
    fn place(&self, _session_id: String) -> BoxFuture<'static, Result<Arc<dyn Placed>, String>> {
        let index = self.calls.fetch_add(1, Ordering::SeqCst).min(self.placements.len() - 1);
        let placed = self.placements[index].clone();
        Box::pin(async move { Ok(placed as Arc<dyn Placed>) })
    }
}

async fn test_placed(entry: &Entry, work: &str, root: &Path, closed: bool) -> TestPlaced {
    TestPlaced {
        catalog: Catalog::from_entries(std::slice::from_ref(entry)),
        agents: test_agents(std::slice::from_ref(entry), "/home/test").await,
        builtins: builtins_input(work, root),
        env: MapEnv::new([("ANTHROPIC_API_KEY".to_owned(), "parity".to_owned())]),
        closed: AtomicBool::new(closed),
    }
}

async fn setup(port: u16) -> Setup {
    setup_placements(port, false).await
}

async fn setup_placements(port: u16, closed_first: bool) -> Setup {
    let dir = TempDir::new();
    let work_path = dir.path().join("work");
    std::fs::create_dir_all(&work_path).expect("create the work directory");
    let work = work_path.to_string_lossy().into_owned();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    let database = Arc::new(database);
    let bus = Bus::new(
        database.clone(),
        BusOptions { persist: true, now: Some(Arc::new(|| FIXED_NOW)), ..Default::default() },
    );
    let _projector = SessionProjector::start(&bus, &database, Arc::new(ProjectionSettings::default));
    database
        .read(|connection| {
            connection.execute(
                "INSERT INTO project (id, worktree, sandboxes, time_created, time_updated) \
                 VALUES ('prj_1', ?1, '[]', 1, 2)",
                [work.as_str()],
            )?;
            connection.execute(
                "INSERT INTO session_v2 (id, project_id, slug, directory, version, time_created, time_updated) \
                 VALUES ('ses_1', 'prj_1', 'slug-1', ?1, '1', 1000, 1000)",
                [work.as_str()],
            )?;
            Ok(())
        })
        .expect("seed the session");

    let store = SessionStore::new(database.clone());
    let entry = Entry::Document(Document {
        path: None,
        info: decode_json::<ConfigEntryDocumentInfo>(&config_value(port).to_string())
            .expect("decode the config document"),
        origin: Origin::File,
    });
    let placements = if closed_first {
        vec![
            Arc::new(test_placed(&entry, &work, dir.path(), true).await),
            Arc::new(test_placed(&entry, &work, dir.path(), false).await),
        ]
    } else {
        vec![Arc::new(test_placed(&entry, &work, dir.path(), false).await)]
    };
    let placement = TestPlacement { placements, calls: AtomicUsize::new(0) };
    let runner = Runner::new(store.clone(), bus.clone(), database.clone(), Arc::new(placement), app());
    let sessions = Sessions::new(
        store.clone(),
        bus.clone(),
        Arc::new(ProjectService::new(database.clone())),
        Arc::new(LocalEnvironment::new()),
        "1".to_owned(),
    )
    .with_execution(Execution::new(bus.clone(), Arc::new(runner)));
    Setup { _dir: dir, database, bus, store, sessions }
}

#[derive(Clone)]
struct Response {
    status: u16,
    reason: &'static str,
    content_type: &'static str,
    extra: &'static str,
    body: String,
    delay: Option<Duration>,
    after_first_delta: bool,
}

impl Response {
    fn cassette() -> Self {
        Self {
            status: 200,
            reason: "OK",
            content_type: "text/event-stream; charset=utf-8",
            extra: "",
            body: cassette_body(),
            delay: None,
            after_first_delta: false,
        }
    }

    fn rejection() -> Self {
        Self {
            status: 500,
            reason: "Internal Server Error",
            content_type: "application/json",
            extra: "x-should-retry: false\r\n",
            body: r#"{"error":{"type":"api_error","message":"boom"}}"#.to_owned(),
            delay: None,
            after_first_delta: false,
        }
    }

    fn delayed(mut self) -> Self {
        self.delay = Some(Duration::from_secs(5));
        self
    }

    fn delayed_after_first_delta(mut self) -> Self {
        self.delay = Some(Duration::from_secs(30));
        self.after_first_delta = true;
        self
    }
}

async fn start_server(response: Response) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind the test server");
    let port = listener.local_addr().expect("the test server has a local address").port();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else { return };
            let response = response.clone();
            tokio::spawn(async move {
                read_request(&mut socket).await;
                let head = format!(
                    "HTTP/1.1 {} {}\r\ncontent-type: {}\r\ncontent-length: {}\r\n{}connection: close\r\n\r\n",
                    response.status,
                    response.reason,
                    response.content_type,
                    response.body.len(),
                    response.extra
                );
                let _ = socket.write_all(head.as_bytes()).await;
                match response.delay {
                    Some(delay) => {
                        let split = if response.after_first_delta {
                            end_of_first_delta(&response.body)
                        } else {
                            response.body.find("content_block_delta").unwrap_or(response.body.len())
                        };
                        let _ = socket.write_all(&response.body.as_bytes()[..split]).await;
                        tokio::time::sleep(delay).await;
                        let _ = socket.write_all(&response.body.as_bytes()[split..]).await;
                    }
                    None => {
                        let _ = socket.write_all(response.body.as_bytes()).await;
                    }
                }
                let _ = socket.shutdown().await;
            });
        }
    });
    port
}

async fn read_request(socket: &mut TcpStream) {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = socket.read(&mut chunk).await.unwrap_or(0);
        if read == 0 {
            return;
        }
        buffer.extend_from_slice(&chunk[..read]);
        let Some(end) = headers_end(&buffer) else { continue };
        let headers = String::from_utf8_lossy(&buffer[..end]).to_lowercase();
        let length = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        while buffer.len() < end + 4 + length {
            let read = socket.read(&mut chunk).await.unwrap_or(0);
            if read == 0 {
                return;
            }
            buffer.extend_from_slice(&chunk[..read]);
        }
        return;
    }
}

fn headers_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn end_of_first_delta(body: &str) -> usize {
    let start = body.find("event: content_block_delta").unwrap_or(body.len());
    body[start..].find("\n\n").map_or(body.len(), |end| start + end + 2)
}

async fn next_event(subscription: &mut Subscription) -> Arc<Event> {
    tokio::time::timeout(Duration::from_secs(10), subscription.recv())
        .await
        .expect("an event before the timeout")
        .expect("the subscription stays open")
}

async fn collect_until(subscription: &mut Subscription, r#type: &str) -> Vec<Arc<Event>> {
    let mut events = Vec::new();
    loop {
        let event = next_event(subscription).await;
        let done = event.r#type() == r#type;
        events.push(event);
        if done {
            return events;
        }
    }
}

fn time_suspended(database: &Database) -> Option<i64> {
    database
        .read(|connection| {
            Ok(connection.query_row("SELECT time_suspended FROM session_v2 WHERE id = ?1", ["ses_1"], |row| {
                row.get::<_, Option<i64>>(0)
            })?)
        })
        .expect("read the session row")
}

fn prompt(sessions: &Sessions) {
    sessions
        .prompt(
            "ses_1",
            PromptInput {
                id: None,
                text: "say hi".to_owned(),
                agents: None,
                metadata: None,
                delivery: SessionInboxDelivery::Steer,
                resume: true,
            },
        )
        .expect("admit the prompt");
}

fn idle_outcome(store: &SessionStore) -> SessionMessageIdleOutcome {
    store
        .messages(&MessagesInput {
            session_id: "ses_1".to_owned(),
            limit: None,
            order: Some(Order::Asc),
            r#type: None,
            cursor: None,
        })
        .expect("read the session messages")
        .iter()
        .rev()
        .find_map(|message| match message {
            SessionMessageInfo::Idle(idle) => Some(idle.outcome),
            _ => None,
        })
        .expect("the session has an idle message")
}

#[tokio::test]
async fn drain_streams_text_and_settles_the_step() {
    let port = start_server(Response::cassette()).await;
    let setup = setup(port).await;
    let mut subscription = setup.bus.subscribe_all(None);
    prompt(&setup.sessions);
    let events = collect_until(&mut subscription, "session.execution.succeeded").await;
    let types = events.iter().map(|event| event.r#type()).collect::<Vec<_>>();
    assert_eq!(
        types,
        [
            "session.inbox.enqueued",
            "session.execution.started",
            "session.instructions.updated",
            "session.inbox.delivered",
            "session.step.started",
            "session.step.first-token",
            "session.text.started",
            "session.text.delta",
            "session.text.ended",
            "session.step.streamed",
            "session.step.ended",
            "session.usage.updated",
            "session.execution.succeeded",
        ]
    );
    let delta =
        events.iter().find(|event| event.r#type() == "session.text.delta").expect("the drain streams a text delta");
    assert_eq!(delta.data()["delta"], json!("Hello!"));

    let messages = setup
        .store
        .messages(&MessagesInput {
            session_id: "ses_1".to_owned(),
            limit: None,
            order: Some(Order::Asc),
            r#type: None,
            cursor: None,
        })
        .expect("read the session messages");
    let assistant = messages
        .iter()
        .rev()
        .find_map(|message| match message {
            SessionMessageInfo::Assistant(assistant) => Some(assistant),
            _ => None,
        })
        .expect("the drain persists an assistant message");
    let value: Value = serde_json::from_str(&encode_json(assistant)).expect("encode the assistant message");
    assert_eq!(value["content"], json!([{ "type": "text", "text": "Hello!" }]));
    assert_eq!(value["finish"], json!("stop"));
    assert_eq!(idle_outcome(&setup.store), SessionMessageIdleOutcome::Succeeded);
    assert_eq!(time_suspended(&setup.database), None);
}

#[tokio::test]
async fn a_closed_placement_reloads_the_location_instead_of_succeeding() {
    let port = start_server(Response::cassette()).await;
    let setup = setup_placements(port, true).await;
    let mut subscription = setup.bus.subscribe_all(None);
    prompt(&setup.sessions);
    let events = collect_until(&mut subscription, "session.execution.succeeded").await;
    let types = events.iter().map(|event| event.r#type()).collect::<Vec<_>>();
    assert!(types.contains(&"session.inbox.delivered"));
    assert!(types.contains(&"session.step.ended"));
    assert_eq!(idle_outcome(&setup.store), SessionMessageIdleOutcome::Succeeded);
    assert_eq!(time_suspended(&setup.database), None);
}

#[tokio::test]
async fn drain_reports_a_rejected_request() {
    let port = start_server(Response::rejection()).await;
    let setup = setup(port).await;
    let mut subscription = setup.bus.subscribe_all(None);
    prompt(&setup.sessions);
    let events = collect_until(&mut subscription, "session.execution.failed").await;
    let types = events.iter().map(|event| event.r#type()).collect::<Vec<_>>();
    assert_eq!(
        &types[..5],
        [
            "session.inbox.enqueued",
            "session.execution.started",
            "session.instructions.updated",
            "session.inbox.delivered",
            "session.step.started",
        ]
    );
    assert_eq!(&types[types.len() - 2..], ["session.step.failed", "session.execution.failed"]);
    assert_eq!(idle_outcome(&setup.store), SessionMessageIdleOutcome::Failed);
    assert_eq!(time_suspended(&setup.database), None);
}

#[tokio::test]
async fn removing_a_running_session_interrupts_the_drain_first() {
    let port = start_server(Response::cassette().delayed()).await;
    let setup = setup(port).await;
    let mut subscription = setup.bus.subscribe_all(None);
    prompt(&setup.sessions);
    let mut events = collect_until(&mut subscription, "session.step.started").await;
    tokio::time::timeout(Duration::from_secs(10), setup.sessions.remove("ses_1"))
        .await
        .expect("removal returns before the timeout")
        .expect("remove the session");
    events.extend(collect_until(&mut subscription, "session.deleted").await);
    let position = |r#type: &str| {
        events.iter().position(|event| event.r#type() == r#type).unwrap_or_else(|| panic!("missing {}", r#type))
    };
    assert!(position("session.execution.interrupted") < position("session.deleted"));
    let interrupted = events
        .iter()
        .find(|event| event.r#type() == "session.execution.interrupted")
        .expect("the drain is interrupted");
    assert_eq!(interrupted.data()["reason"], json!("user"));
    assert!(matches!(setup.sessions.get("ses_1"), Err(SessionError::NotFound(_))));
}

#[tokio::test]
async fn interrupting_a_streaming_step_flushes_partial_output() {
    let port = start_server(Response::cassette().delayed_after_first_delta()).await;
    let setup = setup(port).await;
    let mut subscription = setup.bus.subscribe_all(None);
    prompt(&setup.sessions);
    let mut events = collect_until(&mut subscription, "session.text.delta").await;
    tokio::time::timeout(Duration::from_secs(10), setup.sessions.execution().interrupt("ses_1"))
        .await
        .expect("the interrupt returns before the timeout");
    events.extend(collect_until(&mut subscription, "session.execution.interrupted").await);
    let position = |r#type: &str| {
        events.iter().position(|event| event.r#type() == r#type).unwrap_or_else(|| panic!("missing {}", r#type))
    };
    let ended = events
        .iter()
        .find(|event| event.r#type() == "session.text.ended")
        .expect("the interrupted step ends the open text block");
    assert_eq!(ended.data()["text"], json!("Hello"));
    let failed = events
        .iter()
        .find(|event| event.r#type() == "session.step.failed")
        .expect("the interrupted step is recorded as failed");
    assert_eq!(failed.data()["error"]["type"], json!("aborted"));
    assert_eq!(failed.data()["error"]["message"], json!("Step interrupted"));
    assert!(position("session.text.ended") < position("session.step.failed"));
    assert!(position("session.step.failed") < position("session.execution.interrupted"));
    let interrupted = events
        .iter()
        .find(|event| event.r#type() == "session.execution.interrupted")
        .expect("the drain is interrupted");
    assert_eq!(interrupted.data()["reason"], json!("user"));
}

#[tokio::test]
async fn removing_a_session_waits_for_the_execution_to_go_idle() {
    let setup = setup(0).await;
    let execution = Arc::new(BlockingExecution::new());
    let sessions = Sessions::new(
        setup.store.clone(),
        setup.bus.clone(),
        Arc::new(ProjectService::new(setup.database.clone())),
        Arc::new(LocalEnvironment::new()),
        "1".to_owned(),
    )
    .with_execution(execution.clone());
    let mut deleted = setup.bus.subscribe(&["session.deleted"], None);
    let removing = tokio::spawn(async move { sessions.remove("ses_1").await });
    tokio::time::timeout(Duration::from_secs(10), execution.waiting.notified())
        .await
        .expect("removal waits for the execution to go idle");
    assert!(!removing.is_finished());
    assert!(deleted.try_recv().is_none());
    execution.released.notify_waiters();
    removing.await.expect("the removal task completes").expect("remove the session");
    assert_eq!(*execution.calls.lock().expect("the calls lock is never poisoned"), ["interrupt", "wait_idle"]);
    let event = next_event(&mut deleted).await;
    assert_eq!(event.r#type(), "session.deleted");
}

struct BlockingExecution {
    calls: std::sync::Mutex<Vec<&'static str>>,
    waiting: tokio::sync::Notify,
    released: tokio::sync::Notify,
}

impl BlockingExecution {
    fn new() -> Self {
        Self {
            calls: std::sync::Mutex::new(Vec::new()),
            waiting: tokio::sync::Notify::new(),
            released: tokio::sync::Notify::new(),
        }
    }
}

impl SessionExecution for BlockingExecution {
    fn wake(&self, _session_id: &str) {}

    fn interrupt<'a>(&'a self, _session_id: &'a str) -> BoxFuture<'a, ()> {
        self.calls.lock().expect("the calls lock is never poisoned").push("interrupt");
        Box::pin(async {})
    }

    fn wait_idle<'a>(&'a self, _session_id: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.calls.lock().expect("the calls lock is never poisoned").push("wait_idle");
            self.waiting.notify_one();
            self.released.notified().await;
        })
    }

    fn active(&self) -> Vec<String> {
        Vec::new()
    }

    fn shutdown<'a>(&'a self) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

#[tokio::test]
async fn every_wake_drains_its_prompt() {
    let port = start_server(Response::cassette()).await;
    let setup = setup(port).await;
    let rounds = 20;
    for _ in 0..rounds {
        prompt(&setup.sessions);
        setup.sessions.wait_idle("ses_1").await;
        assert!(setup.sessions.active().is_empty());
    }
    let assistants = setup
        .store
        .messages(&MessagesInput {
            session_id: "ses_1".to_owned(),
            limit: None,
            order: Some(Order::Asc),
            r#type: None,
            cursor: None,
        })
        .expect("read the session messages")
        .iter()
        .filter(|message| matches!(message, SessionMessageInfo::Assistant(_)))
        .count();
    assert_eq!(assistants, rounds);
}
