use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::watch;
use tokio::time::timeout;

use crate::daemon::protocol::{
    AttachmentRole, MAX_FRAME_BYTES, OUTPUT_FRAME_TAG, ProcessInfo, Registration, Request, Response,
};
use crate::daemon::{DaemonError, ErrorKind, Replay, StreamEvent, SubscribeInput};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

pub struct DaemonAttachment {
    pub terminal: ProcessInfo,
    pub role: AttachmentRole,
    pub generation: u64,
    pub replay: Replay,
    on_event: Arc<dyn Fn(StreamEvent) + Send + Sync>,
    on_end: Arc<dyn Fn() + Send + Sync>,
    state: Mutex<AttachmentState>,
}

struct AttachmentState {
    stream: Option<UnixStream>,
    activated: bool,
    detached: bool,
    cancel: Option<watch::Sender<bool>>,
}

impl DaemonAttachment {
    pub fn activate(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.activated || state.detached {
            return;
        }
        state.activated = true;
        let Some(stream) = state.stream.take() else { return };
        let (cancel, receiver) = watch::channel(false);
        state.cancel = Some(cancel);
        let on_event = self.on_event.clone();
        let on_end = self.on_end.clone();
        tokio::spawn(pump(stream, on_event, on_end, receiver));
    }

    pub fn detach(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.detached {
            return;
        }
        state.detached = true;
        state.stream = None;
        if let Some(cancel) = &state.cancel {
            let _ = cancel.send(true);
        }
    }
}

async fn pump(
    mut stream: UnixStream,
    on_event: Arc<dyn Fn(StreamEvent) + Send + Sync>,
    on_end: Arc<dyn Fn() + Send + Sync>,
    mut cancel: watch::Receiver<bool>,
) {
    let detached = loop {
        let frame = tokio::select! {
            biased;
            changed = cancel.changed() => {
                let _ = changed;
                break true;
            }
            frame = read_frame(&mut stream) => match frame {
                Ok(Some(frame)) => frame,
                _ => break false,
            },
        };
        if frame.first() == Some(&OUTPUT_FRAME_TAG) {
            if frame.len() < 17 {
                break false;
            }
            let start = u64::from_be_bytes(frame[1..9].try_into().expect("eight bytes"));
            let end = u64::from_be_bytes(frame[9..17].try_into().expect("eight bytes"));
            on_event(StreamEvent::Output { start, end, data: frame[17..].to_vec() });
            continue;
        }
        let Ok(event) = serde_json::from_slice::<Response>(&frame) else { break false };
        match event {
            Response::Resized { cols, rows, generation, checkpoint_base64 } => {
                on_event(StreamEvent::Resized {
                    cols,
                    rows,
                    generation,
                    checkpoint: decode_base64(&checkpoint_base64),
                });
            }
            Response::ControllerChanged { attachment_id, generation } => {
                on_event(StreamEvent::ControllerChanged { attachment_id, generation });
            }
            Response::TitleChanged { title } => on_event(StreamEvent::TitleChanged { title }),
            Response::ForegroundProcessChanged { process } => {
                on_event(StreamEvent::ForegroundProcessChanged { process });
            }
            Response::Exited { exit_code, final_offset, .. } => {
                on_event(StreamEvent::Exited { exit_code, final_offset });
                break false;
            }
            _ => {}
        }
    };
    if !detached {
        on_end();
    }
}

pub async fn one_shot(registration: &Registration, request: &Request) -> Result<Response, DaemonError> {
    let dispatched = Arc::new(AtomicBool::new(false));
    let flag = dispatched.clone();
    let run = async {
        let payload = encode(&registration.token, request).map_err(|error| DaemonError::protocol(error.to_string()))?;
        let mut stream =
            UnixStream::connect(&registration.socket).await.map_err(|error| DaemonError::connect(error.to_string()))?;
        flag.store(true, Ordering::SeqCst);
        stream.write_all(&payload).await.map_err(|error| DaemonError::response(error.to_string()))?;
        let frame = read_frame(&mut stream).await.map_err(|error| DaemonError::response(error.to_string()))?;
        let Some(frame) = frame else { return Err(DaemonError::response("opencode-pty closed without response")) };
        let response = decode(&frame).map_err(DaemonError::response)?;
        if let Response::Error { message } = response {
            return Err(response_error(message));
        }
        Ok(response)
    };
    match timeout(REQUEST_TIMEOUT, run).await {
        Ok(result) => result,
        Err(_) => Err(DaemonError::new(
            if dispatched.load(Ordering::SeqCst) { ErrorKind::Response } else { ErrorKind::Connect },
            "opencode-pty request timed out",
        )),
    }
}

pub async fn wait(registration: &Registration, id: u64) -> Result<Response, DaemonError> {
    let mut stream =
        UnixStream::connect(&registration.socket).await.map_err(|error| DaemonError::connect(error.to_string()))?;
    let payload =
        encode(&registration.token, &Request::Wait { id }).map_err(|error| DaemonError::connect(error.to_string()))?;
    stream.write_all(&payload).await.map_err(|error| DaemonError::connect(error.to_string()))?;
    let frame = read_frame(&mut stream).await.map_err(|error| DaemonError::connect(error.to_string()))?;
    let Some(frame) = frame else {
        return Err(DaemonError::connect("opencode-pty closed before the process exited"));
    };
    let response = decode(&frame).map_err(DaemonError::connect)?;
    match response {
        Response::Error { message } => Err(response_error(message)),
        Response::Exited { .. } => Ok(response),
        other => Err(DaemonError::response(format!("unexpected opencode-pty response: {}", type_name(&other)))),
    }
}

pub async fn subscribe(
    registration: &Registration,
    id: u64,
    input: SubscribeInput,
) -> Result<DaemonAttachment, DaemonError> {
    let mut stream =
        UnixStream::connect(&registration.socket).await.map_err(|error| DaemonError::connect(error.to_string()))?;
    let request = Request::Subscribe {
        id,
        offset: input.cursor,
        attachment_id: input.attachment_id.clone(),
        role: input.role,
        takeover: input.takeover,
    };
    let payload = encode(&registration.token, &request).map_err(|error| DaemonError::connect(error.to_string()))?;
    stream.write_all(&payload).await.map_err(|error| DaemonError::connect(error.to_string()))?;
    let frame = read_frame(&mut stream).await.map_err(|error| DaemonError::connect(error.to_string()))?;
    let Some(frame) = frame else {
        return Err(DaemonError::connect("opencode-pty closed before attachment"));
    };
    let response = decode(&frame).map_err(DaemonError::connect)?;
    match response {
        Response::Error { message } => Err(response_error(message)),
        Response::Attached {
            process,
            role,
            generation,
            requested_offset,
            available_offset,
            end_offset,
            truncated,
            replay_base64,
        } => Ok(DaemonAttachment {
            terminal: process,
            role,
            generation,
            replay: Replay {
                requested_offset,
                available_offset,
                end_offset,
                truncated,
                data: decode_base64(&replay_base64),
            },
            on_event: input.on_event,
            on_end: input.on_end,
            state: Mutex::new(AttachmentState {
                stream: Some(stream),
                activated: false,
                detached: false,
                cancel: None,
            }),
        }),
        other => Err(DaemonError::connect(format!("unexpected opencode-pty response: {}", type_name(&other)))),
    }
}

pub async fn read_frame(reader: &mut (impl AsyncRead + Unpin)) -> io::Result<Option<Vec<u8>>> {
    let mut header = [0u8; 4];
    let mut read = 0;
    while read < header.len() {
        let count = reader.read(&mut header[read..]).await?;
        if count == 0 {
            if read == 0 {
                return Ok(None);
            }
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "opencode-pty truncated frame"));
        }
        read += count;
    }
    let length = u32::from_be_bytes(header) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "opencode-pty frame too large"));
    }
    let mut payload = vec![0; length];
    reader.read_exact(&mut payload).await?;
    Ok(Some(payload))
}

pub async fn write_frame(writer: &mut (impl AsyncWrite + Unpin), token: &str, request: &Request) -> io::Result<()> {
    let payload = encode(token, request)?;
    writer.write_all(&payload).await
}

pub(crate) fn encode(token: &str, request: &Request) -> io::Result<Vec<u8>> {
    let envelope = crate::daemon::protocol::Envelope { token: token.to_owned(), request: request.clone() };
    let payload = serde_json::to_vec(&envelope).map_err(io::Error::other)?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "opencode-pty frame too large"));
    }
    let length = u32::try_from(payload.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "opencode-pty frame too large"))?;
    let mut frame = Vec::with_capacity(payload.len() + 4);
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

fn decode(payload: &[u8]) -> Result<Response, String> {
    serde_json::from_slice(payload).map_err(|error| error.to_string())
}

fn decode_base64(value: &str) -> Vec<u8> {
    STANDARD.decode(value).unwrap_or_default()
}

fn response_error(message: String) -> DaemonError {
    if message == "authentication failed" { DaemonError::registration(message) } else { DaemonError::request(message) }
}

fn type_name(response: &Response) -> &'static str {
    match response {
        Response::Owned => "owned",
        Response::Handoff { .. } => "handoff",
        Response::Pong { .. } => "pong",
        Response::Created { .. } => "created",
        Response::Process { .. } => "process",
        Response::Processes { .. } => "processes",
        Response::Ok => "ok",
        Response::Snapshot { .. } => "snapshot",
        Response::Rows { .. } => "rows",
        Response::Replay { .. } => "replay",
        Response::Attached { .. } => "attached",
        Response::Resized { .. } => "resized",
        Response::Exited { .. } => "exited",
        Response::ControllerChanged { .. } => "controller_changed",
        Response::TitleChanged { .. } => "title_changed",
        Response::ForegroundProcessChanged { .. } => "foreground_process_changed",
        Response::Error { .. } => "error",
    }
}
