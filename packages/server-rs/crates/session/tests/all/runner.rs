use std::path::{Path, PathBuf};
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
use opencode_tool::{ToolContext, ToolError, ToolOptions, ToolOutput, ToolRegistration, ToolRuntime, ToolSpec};
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
    work: String,
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

fn tool_body(name: &str, input: &str, stop_reason: &str) -> String {
    let input = serde_json::to_string(input).expect("the partial input serializes");
    format!(
        "event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"id\":\"msg_test\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"test-model\",\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{{\"input_tokens\":10,\"output_tokens\":0,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0}}}}}}\n\nevent: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"tool_use\",\"id\":\"call_1\",\"name\":\"{name}\",\"input\":{{}}}}}}\n\nevent: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"input_json_delta\",\"partial_json\":{input}}}}}\n\nevent: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":0}}\n\nevent: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"{stop_reason}\"}},\"usage\":{{\"input_tokens\":10,\"output_tokens\":5,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0}}}}\n\nevent: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n"
    )
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
    tools: Arc<opencode_tool::LocationTools>,
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

    fn tools(&self) -> &opencode_tool::LocationTools {
        &self.tools
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

async fn test_placed(
    entry: &Entry,
    work: &str,
    root: &Path,
    closed: bool,
    tools: Arc<opencode_tool::LocationTools>,
) -> TestPlaced {
    TestPlaced {
        catalog: Catalog::from_entries(std::slice::from_ref(entry)),
        agents: test_agents(std::slice::from_ref(entry), "/home/test").await,
        builtins: builtins_input(work, root),
        tools,
        env: MapEnv::new([("ANTHROPIC_API_KEY".to_owned(), "parity".to_owned())]),
        closed: AtomicBool::new(closed),
    }
}

type ToolFactory = Arc<
    dyn Fn(String, PathBuf) -> futures_util::future::BoxFuture<'static, Arc<opencode_tool::LocationTools>>
        + Send
        + Sync,
>;

async fn setup(port: u16) -> Setup {
    setup_placements(port, false).await
}

async fn setup_placements(port: u16, closed_first: bool) -> Setup {
    setup_placements_with(port, closed_first, None).await
}

async fn setup_placements_with(port: u16, closed_first: bool, factory: Option<ToolFactory>) -> Setup {
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
    let tools = match &factory {
        Some(factory) => factory(work.clone(), dir.path().to_path_buf()).await,
        None => crate::test_tools(&work, dir.path(), Arc::new(crate::AllowAll), false).await,
    };
    let placements = if closed_first {
        vec![
            Arc::new(test_placed(&entry, &work, dir.path(), true, tools.clone()).await),
            Arc::new(test_placed(&entry, &work, dir.path(), false, tools).await),
        ]
    } else {
        vec![Arc::new(test_placed(&entry, &work, dir.path(), false, tools).await)]
    };
    let placement = TestPlacement { placements, calls: AtomicUsize::new(0) };
    let runner = Runner::new(store.clone(), bus.clone(), database.clone(), Arc::new(placement), app());
    let sessions = Sessions::new(
        store.clone(),
        bus.clone(),
        Arc::new(ProjectService::new(
            database.clone(),
            Arc::new(opencode_git::Git::new(Arc::new(|name: &str| std::env::var_os(name)))),
        )),
        Arc::new(LocalEnvironment::new()),
        "1".to_owned(),
    )
    .with_execution(Execution::new(bus.clone(), Arc::new(runner)));
    Setup { _dir: dir, database, bus, store, sessions, work }
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

type Captured = Arc<std::sync::Mutex<Vec<Value>>>;

async fn start_server(response: Response) -> u16 {
    start_sequence(vec![response], None).await
}

async fn start_sequence(responses: Vec<Response>, captured: Option<Captured>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind the test server");
    let port = listener.local_addr().expect("the test server has a local address").port();
    let responses = Arc::new(std::sync::Mutex::new(responses.into_iter().rev().collect::<Vec<_>>()));
    let last = Arc::new(std::sync::Mutex::new(None));
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else { return };
            let response = {
                let response = responses.lock().expect("the responses lock is never poisoned").pop();
                if let Some(response) = &response {
                    *last.lock().expect("the last response lock is never poisoned") = Some(response.clone());
                }
                response.or_else(|| last.lock().expect("the last response lock is never poisoned").clone())
            };
            let captured = captured.clone();
            tokio::spawn(async move {
                let request = read_request(&mut socket).await;
                if let Some(captured) = captured
                    && let Some(body) = request
                {
                    captured.lock().expect("the captured lock is never poisoned").push(body);
                }
                let Some(response) = response else { return };
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

async fn read_request(socket: &mut TcpStream) -> Option<Value> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = socket.read(&mut chunk).await.unwrap_or(0);
        if read == 0 {
            return None;
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
                return None;
            }
            buffer.extend_from_slice(&chunk[..read]);
        }
        return serde_json::from_slice::<Value>(&buffer[end + 4..end + 4 + length]).ok();
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
        Arc::new(ProjectService::new(
            setup.database.clone(),
            Arc::new(opencode_git::Git::new(Arc::new(|name: &str| std::env::var_os(name)))),
        )),
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
async fn a_tool_call_runs_and_the_next_step_carries_its_result() {
    let captured: Captured = Arc::new(std::sync::Mutex::new(Vec::new()));
    let port = start_sequence(
        vec![
            Response {
                status: 200,
                reason: "OK",
                content_type: "text/event-stream; charset=utf-8",
                extra: "",
                body: tool_body("read", &json!({ "path": "hello.txt" }).to_string(), "tool_use"),
                delay: None,
                after_first_delta: false,
            },
            Response::cassette(),
        ],
        Some(captured.clone()),
    )
    .await;
    let setup = setup(port).await;
    std::fs::write(Path::new(&setup.work).join("hello.txt"), "hello\n").expect("write the fixture");
    let mut subscription = setup.bus.subscribe_all(None);
    prompt(&setup.sessions);
    let events = collect_until(&mut subscription, "session.execution.succeeded").await;
    let position = |r#type: &str| {
        events.iter().position(|event| event.r#type() == r#type).unwrap_or_else(|| panic!("missing {}", r#type))
    };
    assert!(position("session.tool.input.started") < position("session.tool.called"));
    assert!(position("session.tool.called") < position("session.step.streamed"));
    assert!(position("session.step.streamed") < position("session.tool.success"));
    assert!(position("session.tool.success") < position("session.step.ended"));
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
    let tool = messages.iter().find_map(|message| match message {
        SessionMessageInfo::Assistant(assistant) => assistant.content.iter().find_map(|item| match item {
            opencode_contract::generated::SessionMessageAssistantContentItem::Tool(tool) => Some(tool),
            _ => None,
        }),
        _ => None,
    });
    let tool = tool.expect("the assistant records the tool call");
    assert!(matches!(tool.state, opencode_contract::generated::SessionMessageAssistantTool1State::Completed(_)));
    let requests = captured.lock().expect("the captured lock is never poisoned");
    assert_eq!(requests.len(), 2, "the tool result triggers a second request");
    let second = requests[1].to_string();
    assert!(second.contains("tool_use"), "the second request carries the call");
    assert!(second.contains("tool_result"), "the second request carries the result");
    assert!(second.contains("hello"), "the second request carries the file content");
}

#[tokio::test]
async fn an_unknown_tool_is_recorded_as_a_tool_failure() {
    let port = start_sequence(
        vec![
            Response {
                status: 200,
                reason: "OK",
                content_type: "text/event-stream; charset=utf-8",
                extra: "",
                body: tool_body("does_not_exist", "{}", "tool_use"),
                delay: None,
                after_first_delta: false,
            },
            Response::cassette(),
        ],
        None,
    )
    .await;
    let setup = setup(port).await;
    let mut subscription = setup.bus.subscribe_all(None);
    prompt(&setup.sessions);
    let events = collect_until(&mut subscription, "session.execution.succeeded").await;
    let failed = events.iter().find(|event| event.r#type() == "session.tool.failed").expect("the unknown tool fails");
    assert_eq!(failed.data()["error"]["type"], json!("tool.execution"));
    assert!(
        failed.data()["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("No tool named \"does_not_exist\""))
    );
}

#[tokio::test]
async fn malformed_tool_input_is_recorded_with_the_exact_message() {
    let port = start_sequence(
        vec![
            Response {
                status: 200,
                reason: "OK",
                content_type: "text/event-stream; charset=utf-8",
                extra: "",
                body: tool_body("read", "{\"path\":", "tool_use"),
                delay: None,
                after_first_delta: false,
            },
            Response::cassette(),
        ],
        None,
    )
    .await;
    let setup = setup(port).await;
    let mut subscription = setup.bus.subscribe_all(None);
    prompt(&setup.sessions);
    let events = collect_until(&mut subscription, "session.execution.succeeded").await;
    let failed = events
        .iter()
        .find(|event| event.r#type() == "session.tool.failed")
        .expect("the malformed input fails the call");
    assert_eq!(failed.data()["error"]["type"], json!("tool.input-json"));
    let message = failed.data()["error"]["message"].as_str().expect("the failure carries a message");
    assert!(message.starts_with("The tool input could not be parsed as JSON"));
    assert!(message.contains("Input excerpt (first"), "the failure carries the bounded excerpt");
}

#[tokio::test]
async fn a_denied_permission_is_recorded_as_a_permission_rejection() {
    let port = start_sequence(
        vec![
            Response {
                status: 200,
                reason: "OK",
                content_type: "text/event-stream; charset=utf-8",
                extra: "",
                body: tool_body("read", &json!({ "path": "hello.txt" }).to_string(), "tool_use"),
                delay: None,
                after_first_delta: false,
            },
            Response::cassette(),
        ],
        None,
    )
    .await;
    let factory: ToolFactory = Arc::new(|work, root| {
        Box::pin(async move { crate::test_tools(&work, &root, Arc::new(crate::DenyAll), false).await })
    });
    let setup = setup_placements_with(port, false, Some(factory)).await;
    let mut subscription = setup.bus.subscribe_all(None);
    prompt(&setup.sessions);
    let events = collect_until(&mut subscription, "session.execution.succeeded").await;
    let failed = events.iter().find(|event| event.r#type() == "session.tool.failed").expect("the denied call fails");
    assert_eq!(failed.data()["error"]["type"], json!("permission.rejected"));
    assert!(failed.data()["error"]["message"].as_str().is_some_and(|message| message.contains("Permission denied")));
}

struct SlowTool {
    started: Arc<tokio::sync::Notify>,
}

impl ToolRuntime for SlowTool {
    fn execute<'a>(&'a self, _input: Value, _context: &'a ToolContext) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        Box::pin(async move {
            self.started.notify_waiters();
            tokio::time::sleep(Duration::from_secs(3_600)).await;
            Err(ToolError::new("the slow tool was not interrupted"))
        })
    }
}

#[tokio::test]
async fn interrupting_a_running_tool_settles_it_as_aborted() {
    let port = start_sequence(
        vec![Response {
            status: 200,
            reason: "OK",
            content_type: "text/event-stream; charset=utf-8",
            extra: "",
            body: tool_body("slow", "{}", "tool_use"),
            delay: None,
            after_first_delta: false,
        }],
        None,
    )
    .await;
    let started = Arc::new(tokio::sync::Notify::new());
    let factory: ToolFactory = {
        let started = started.clone();
        Arc::new(move |work, root| {
            let started = started.clone();
            Box::pin(async move {
                let tools = crate::test_tools(&work, &root, Arc::new(crate::AllowAll), false).await;
                let registration = tools
                    .registry()
                    .transform(move |editor| {
                        let started = started.clone();
                        editor.add(ToolRegistration {
                            spec: ToolSpec {
                                name: "slow".to_owned(),
                                description: Some("A tool that waits until it is interrupted".to_owned()),
                                input_schema: Some(json!({ "type": "object" })),
                                ..ToolSpec::default()
                            },
                            options: ToolOptions::default(),
                            runtime: Arc::new(SlowTool { started }),
                        });
                    })
                    .await;
                std::mem::forget(registration);
                tools
            })
        })
    };
    let setup = setup_placements_with(port, false, Some(factory)).await;
    let mut subscription = setup.bus.subscribe_all(None);
    prompt(&setup.sessions);
    tokio::time::timeout(Duration::from_secs(10), started.notified())
        .await
        .expect("the tool starts before the timeout");
    tokio::time::timeout(Duration::from_secs(10), setup.sessions.execution().interrupt("ses_1"))
        .await
        .expect("the interrupt returns before the timeout");
    let events = collect_until(&mut subscription, "session.execution.interrupted").await;
    let failed = events.iter().find(|event| event.r#type() == "session.tool.failed").expect("the running tool fails");
    assert_eq!(failed.data()["error"]["type"], json!("aborted"));
    assert_eq!(failed.data()["error"]["message"], json!("Tool execution interrupted"));
    let step = events.iter().find(|event| event.r#type() == "session.step.failed").expect("the interrupted step fails");
    assert_eq!(step.data()["error"]["message"], json!("Step interrupted"));
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
