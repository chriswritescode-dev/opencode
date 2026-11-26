use std::path::{Path, PathBuf};
use std::sync::Arc;

use opencode_terminal::daemon::protocol::{Envelope, PROTOCOL_VERSION, Registration, Request, Response};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, watch};

#[allow(clippy::large_enum_variant)]
pub enum Action {
    Reply(Response),
    Hold,
}

#[derive(Clone)]
pub struct Connection {
    frames: mpsc::UnboundedSender<Vec<u8>>,
    close: mpsc::UnboundedSender<()>,
}

impl Connection {
    pub fn send(&self, frame: Vec<u8>) {
        let _ = self.frames.send(frame);
    }

    pub fn close(&self) {
        let _ = self.close.send(());
    }
}

pub type Handler = Arc<dyn Fn(&Request, &str, Connection) -> Action + Send + Sync>;

pub struct FakeServer {
    path: PathBuf,
    _task: tokio::task::JoinHandle<()>,
    _shutdown: watch::Sender<bool>,
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
    FakeServer { path: path.to_owned(), _task: task, _shutdown: shutdown }
}

async fn connection(mut stream: UnixStream, handler: Handler, mut shutdown: watch::Receiver<bool>) {
    let (frames, mut outgoing) = mpsc::unbounded_channel::<Vec<u8>>();
    let (close, mut closing) = mpsc::unbounded_channel::<()>();
    let connection = Connection { frames, close };
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = closing.recv() => break,
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
                }
            },
        }
    }
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
