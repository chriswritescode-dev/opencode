#![allow(dead_code)]
#![allow(clippy::result_large_err)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::protocol::frame::CloseFrame;

const PAGE: usize = 200;

enum Command {
    Events(Vec<Value>),
    Close(u16, String),
    Shutdown,
}

#[derive(Default)]
struct State {
    upgrade: bool,
    opens: usize,
    inbox: Vec<Value>,
    acks: Vec<Vec<String>>,
    sockets: Vec<mpsc::UnboundedSender<Command>>,
}

/// A loopback event-watch stream: Console's WebSocket upgrade, snapshots of the unacknowledged events, close control
/// and the acknowledgements it reads back.
pub struct FakeConsole {
    origin: String,
    state: Arc<Mutex<State>>,
    task: JoinHandle<()>,
}

impl FakeConsole {
    pub async fn start(upgrade: bool) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind the fake Console");
        let origin = format!("http://{}", listener.local_addr().expect("the fake Console address"));
        let state = Arc::new(Mutex::new(State { upgrade, ..Default::default() }));
        let task = tokio::spawn(serve(listener, state.clone()));
        Self { origin, state, task }
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn opens(&self) -> usize {
        self.state.lock().expect("lock the fake Console").opens
    }

    pub fn acks(&self) -> Vec<Vec<String>> {
        self.state.lock().expect("lock the fake Console").acks.clone()
    }

    pub fn socket_count(&self) -> usize {
        self.state.lock().expect("lock the fake Console").sockets.len()
    }

    pub fn push(&self, events: Vec<Value>) {
        let mut state = self.state.lock().expect("lock the fake Console");
        state.inbox.extend(events.clone());
        state.sockets.retain(|socket| socket.send(Command::Events(events.clone())).is_ok());
    }

    pub fn set_inbox(&self, events: Vec<Value>) {
        self.state.lock().expect("lock the fake Console").inbox = events;
    }

    pub fn close_sockets(&self, code: u16) {
        let state = self.state.lock().expect("lock the fake Console");
        for socket in &state.sockets {
            let _ = socket.send(Command::Close(code, "closing".to_owned()));
        }
    }
}

impl Drop for FakeConsole {
    fn drop(&mut self) {
        let state = self.state.lock().expect("lock the fake Console");
        for socket in &state.sockets {
            let _ = socket.send(Command::Shutdown);
        }
        self.task.abort();
    }
}

async fn serve(listener: TcpListener, state: Arc<Mutex<State>>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let state = state.clone();
        tokio::spawn(async move {
            if !state.lock().expect("lock the fake Console").upgrade {
                let _ = tokio_tungstenite::accept_hdr_async(stream, |_: &Request, _: Response| {
                    Err(ErrorResponse::new(Some("Expected a WebSocket upgrade.".to_owned())))
                })
                .await;
                return;
            }
            let Ok(socket) = tokio_tungstenite::accept_async(stream).await else {
                return;
            };
            let (sender, mut commands) = mpsc::unbounded_channel();
            {
                let mut state = state.lock().expect("lock the fake Console");
                state.opens += 1;
                state.sockets.push(sender.clone());
            }
            handle(socket, state.clone(), &mut commands).await;
            state.lock().expect("lock the fake Console").sockets.retain(|socket| !socket.same_channel(&sender));
        });
    }
}

async fn handle(
    socket: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    state: Arc<Mutex<State>>,
    commands: &mut mpsc::UnboundedReceiver<Command>,
) {
    let (mut sink, mut stream) = socket.split();
    let inbox = state.lock().expect("lock the fake Console").inbox.clone();
    for frame in frames(inbox) {
        if sink.send(frame_message("snapshot", frame)).await.is_err() {
            return;
        }
    }
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::Events(events)) => {
                    if sink.send(frame_message("events", events)).await.is_err() {
                        return;
                    }
                }
                Some(Command::Close(code, reason)) => {
                    let frame = CloseFrame { code: code.into(), reason: reason.into() };
                    let _ = sink.send(Message::Close(Some(frame))).await;
                    let _ = sink.close().await;
                    return;
                }
                Some(Command::Shutdown) | None => {
                    let _ = sink.close().await;
                    return;
                }
            },
            message = stream.next() => match message {
                Some(Ok(Message::Text(text))) => acknowledge(&state, text.as_str()),
                Some(Ok(Message::Close(_))) | None => {
                    let _ = sink.close().await;
                    return;
                }
                Some(Err(_)) => return,
                _ => {}
            }
        }
    }
}

fn frame_message(kind: &str, events: Vec<Value>) -> Message {
    Message::text(serde_json::to_string(&json!({ "type": kind, "events": events })).expect("a fake frame serializes"))
}

fn frames(events: Vec<Value>) -> Vec<Vec<Value>> {
    if events.is_empty() {
        return vec![Vec::new()];
    }
    let mut frames: Vec<Vec<Value>> = events.chunks(PAGE).map(|frame| frame.to_vec()).collect();
    if frames.is_empty() {
        frames.push(Vec::new());
    }
    frames
}

fn acknowledge(state: &Mutex<State>, text: &str) {
    let Ok(body) = serde_json::from_str::<Value>(text) else {
        return;
    };
    let Some(ids) = body.get("ids").and_then(Value::as_array) else {
        return;
    };
    let ids: Vec<String> = ids.iter().filter_map(Value::as_str).map(str::to_owned).collect();
    let mut state = state.lock().expect("lock the fake Console");
    state.acks.push(ids.clone());
    state.inbox.retain(|event| {
        let id = event.get("id").and_then(Value::as_str).unwrap_or_default();
        !ids.iter().any(|acked| acked == id)
    });
}

pub async fn wait_until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if condition() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the condition settles");
}
