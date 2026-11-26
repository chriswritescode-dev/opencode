use std::path::{Path, PathBuf};
use std::sync::Arc;

use opencode_terminal::daemon::protocol::{
    Envelope, PROTOCOL_VERSION, ProcessInfo, ProcessMode, PtyInfo, Registration, Request, Response, TerminalLifecycle,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, watch};

#[allow(clippy::large_enum_variant)]
pub enum Action {
    Reply(Response),
    Hold,
    Close,
}

#[derive(Clone)]
pub struct Connection {
    frames: mpsc::UnboundedSender<Vec<u8>>,
    closed: watch::Receiver<bool>,
}

impl Connection {
    pub fn send(&self, frame: Vec<u8>) {
        let _ = self.frames.send(frame);
    }

    pub async fn wait_closed(&self) {
        let mut closed = self.closed.clone();
        if *closed.borrow() {
            return;
        }
        while closed.changed().await.is_ok() {
            if *closed.borrow() {
                return;
            }
        }
    }
}

pub type Handler = Arc<dyn Fn(&Request, &str, Connection) -> Action + Send + Sync>;

pub struct FakeServer {
    path: PathBuf,
    task: tokio::task::JoinHandle<()>,
    shutdown: watch::Sender<bool>,
}

impl FakeServer {
    pub async fn close(self) {
        let _ = self.shutdown.send(true);
        self.task.abort();
        let _ = tokio::fs::remove_file(&self.path).await;
        tokio::task::yield_now().await;
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub async fn listen(path: &Path, handler: Handler) -> FakeServer {
    let _ = tokio::fs::remove_file(path).await;
    let listener = UnixListener::bind(path).expect("bind the fake daemon socket");
    let (shutdown, mut receiver) = watch::channel(false);
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = receiver.changed() => break,
                accepted = listener.accept() => match accepted {
                    Ok((stream, _)) => {
                        let handler = handler.clone();
                        let shutdown = receiver.clone();
                        tokio::spawn(connection(stream, handler, shutdown));
                    }
                    Err(_) => break,
                },
            }
        }
    });
    FakeServer { path: path.to_owned(), task, shutdown }
}

async fn connection(mut stream: UnixStream, handler: Handler, mut shutdown: watch::Receiver<bool>) {
    let (frames, mut outgoing) = mpsc::unbounded_channel::<Vec<u8>>();
    let (closed, closed_rx) = watch::channel(false);
    let connection = Connection { frames, closed: closed_rx };
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            frame = outgoing.recv() => match frame {
                Some(frame) => {
                    if stream.write_all(&frame).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
            incoming = read_frame(&mut stream) => {
                let Some(payload) = incoming else { break };
                let Ok(envelope) = serde_json::from_slice::<Envelope>(&payload) else { break };
                if envelope.request.op() == "own" {
                    if stream.write_all(&frame(&Response::Owned)).await.is_err() {
                        break;
                    }
                    continue;
                }
                match handler(&envelope.request, &envelope.token, connection.clone()) {
                    Action::Reply(response) => {
                        if stream.write_all(&frame(&response)).await.is_err() {
                            break;
                        }
                    }
                    Action::Hold => {}
                    Action::Close => break,
                }
            },
        }
    }
    let _ = closed.send(true);
}

async fn read_frame(stream: &mut UnixStream) -> Option<Vec<u8>> {
    let mut header = [0u8; 4];
    stream.read_exact(&mut header).await.ok()?;
    let length = u32::from_be_bytes(header) as usize;
    let mut payload = vec![0; length];
    stream.read_exact(&mut payload).await.ok()?;
    Some(payload)
}

pub fn frame(value: &Response) -> Vec<u8> {
    let payload = serde_json::to_vec(value).expect("serialize a fake daemon frame");
    let mut output = Vec::with_capacity(payload.len() + 4);
    output.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    output.extend_from_slice(&payload);
    output
}

pub async fn write_registration(directory: &Path, socket: &Path, instance: &str, token: &str) {
    let registration = Registration {
        instance_id: instance.to_owned(),
        pid: std::process::id(),
        protocol: PROTOCOL_VERSION,
        socket: socket.to_owned(),
        token: token.to_owned(),
        features: Vec::new(),
    };
    tokio::fs::write(
        directory.join("service.json"),
        serde_json::to_vec(&registration).expect("serialize the registration"),
    )
    .await
    .expect("write the registration");
}

pub fn terminal(id: u64) -> ProcessInfo {
    ProcessInfo {
        id,
        mode: ProcessMode::Pty,
        pid: None,
        command: vec!["test".to_owned()],
        cwd: PathBuf::from("/tmp"),
        group_id: "ses_test".to_owned(),
        lifecycle: TerminalLifecycle::Running,
        output_head: 0,
        output_tail: 0,
        started_at: 0,
        exited_at: None,
        pty: Some(PtyInfo { title: "test".to_owned(), user_title: None, foreground_process: None, cols: 80, rows: 24 }),
        pipe: None,
    }
}
