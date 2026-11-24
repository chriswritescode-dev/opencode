//! The live connection to the host: framed messages both ways, calls, credit-based streams, cancellation and the
//! limits that bound every queue.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

use bytes::Bytes;
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use opencode_util::lock;

use crate::config::Limits;
use crate::error::SidecarError;
use crate::frame::{self, Broken, Frame, FrameError};
use crate::protocol::{LocationRef, LogLevel, Message, RemoteError};

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub method: String,
    pub location: Option<LocationRef>,
    pub params: Value,
    pub bytes: Option<Bytes>,
}

impl Request {
    pub fn new(method: impl Into<String>, params: Value) -> Self {
        Self { method: method.into(), location: None, params, bytes: None }
    }

    pub fn with_location(self, location: LocationRef) -> Self {
        Self { location: Some(location), ..self }
    }

    pub fn with_bytes(self, bytes: Bytes) -> Self {
        Self { bytes: Some(bytes), ..self }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    pub value: Value,
    pub bytes: Option<Bytes>,
}

impl Reply {
    pub fn new(value: Value) -> Self {
        Self { value, bytes: None }
    }
}

pub type Handler = Arc<dyn Fn(CallContext, Request) -> BoxFuture<Result<Reply, RemoteError>> + Send + Sync>;
pub type StreamHandler =
    Arc<dyn Fn(CallContext, Request, ChunkSink) -> BoxFuture<Result<(), RemoteError>> + Send + Sync>;

#[derive(Clone, Default)]
pub struct Handlers {
    calls: HashMap<String, Handler>,
    streams: HashMap<String, StreamHandler>,
}

impl Handlers {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &mut self,
        method: &str,
        handler: impl Fn(CallContext, Request) -> BoxFuture<Result<Reply, RemoteError>> + Send + Sync + 'static,
    ) {
        self.calls.insert(method.to_owned(), Arc::new(handler));
    }

    pub fn register_stream(
        &mut self,
        method: &str,
        handler: impl Fn(CallContext, Request, ChunkSink) -> BoxFuture<Result<(), RemoteError>> + Send + Sync + 'static,
    ) {
        self.streams.insert(method.to_owned(), Arc::new(handler));
    }
}

#[derive(Clone)]
pub struct CallContext {
    pub id: u64,
    pub location: Option<LocationRef>,
    pub parent: Option<u64>,
    pub cancel: CancellationToken,
    pub client: Client,
}

#[derive(Clone)]
pub struct Client {
    connection: Connection,
    location: Option<LocationRef>,
    parent: u64,
}

impl Client {
    pub fn location(&self) -> Option<&LocationRef> {
        self.location.as_ref()
    }

    pub async fn call(&self, request: Request) -> Result<Reply, SidecarError> {
        self.connection.invoke(self.inherit(request), Some(self.parent)).await
    }

    pub async fn stream(&self, request: Request) -> Result<ReplyStream, SidecarError> {
        self.connection.open(self.inherit(request), Some(self.parent)).await
    }

    fn inherit(&self, request: Request) -> Request {
        if request.location.is_some() {
            return request;
        }
        Request { location: self.location.clone(), ..request }
    }
}

#[derive(Clone)]
pub struct Connection {
    inner: Arc<Inner>,
}

impl Connection {
    /// Both tasks hold a `Weak<Inner>` deliberately. Holding an `Arc` would keep `Inner` and with it the frame
    /// sender alive, so dropping the last `Connection` would never close our write half and the host would never
    /// see the end of its input.
    pub fn start(stream: UnixStream, limits: &Limits, handlers: Handlers) -> Connection {
        let (read, write) = stream.into_split();
        let budget = limits.queued_bytes.max(limits.max_frame).min(u32::MAX as usize);
        let (frames, queued) = mpsc::channel::<Framed>(64);
        let inner = Arc::new(Inner {
            frames,
            room: Arc::new(Semaphore::new(budget)),
            budget: budget as u32,
            window: limits.window,
            max_frame: limits.max_frame,
            max_outgoing: limits.max_outgoing,
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            dead: Mutex::new(None),
            pong: Mutex::new(None),
            handlers,
            incoming: Mutex::new(HashMap::new()),
            credits: Mutex::new(HashMap::new()),
            slots: Arc::new(Semaphore::new(limits.max_incoming)),
            refusals: Arc::new(Semaphore::new(limits.max_incoming)),
        });
        tokio::spawn(write_frames(write, queued, Arc::downgrade(&inner)));
        tokio::spawn(read_frames(read, Arc::downgrade(&inner), limits.max_frame));
        Connection { inner }
    }

    pub async fn call(&self, request: Request) -> Result<Reply, SidecarError> {
        self.invoke(request, None).await
    }

    pub async fn stream(&self, request: Request) -> Result<ReplyStream, SidecarError> {
        self.open(request, None).await
    }

    pub async fn ping(&self, seq: u64) -> Result<(), SidecarError> {
        self.inner.send(Message::Ping { seq }, None).await
    }

    pub fn pong(&self) -> Option<Pong> {
        *lock(&self.inner.pong)
    }

    async fn invoke(&self, request: Request, parent: Option<u64>) -> Result<Reply, SidecarError> {
        let (answer, reply) = oneshot::channel();
        let id = self.inner.register(Pending::Call(answer))?;
        let _guard = CallGuard { connection: self.clone(), id };
        let message = Message::Call {
            id,
            method: request.method,
            location: request.location,
            parent,
            params: request.params,
            stream: false,
        };
        if let Err(error) = self.inner.send(message, request.bytes.as_deref()).await {
            self.inner.remove(id);
            return Err(error);
        }
        match reply.await {
            Ok(outcome) => outcome,
            Err(_) => Err(SidecarError::ended()),
        }
    }

    async fn open(&self, request: Request, parent: Option<u64>) -> Result<ReplyStream, SidecarError> {
        let window = self.inner.window.max(1) as usize;
        let (chunks, queued) = mpsc::channel(window);
        let (answer, outcome) = oneshot::channel();
        let id = self.inner.register(Pending::Stream { chunks, done: Some(answer), sent: 0, taken: 0 })?;
        let stream =
            ReplyStream { connection: self.clone(), id, chunks: queued, done: Some(outcome), finished: false, owed: 0 };
        let message = Message::Call {
            id,
            method: request.method,
            location: request.location,
            parent,
            params: request.params,
            stream: true,
        };
        self.inner.send(message, request.bytes.as_deref()).await?;
        self.inner.send(Message::Credit { id, chunks: self.inner.window }, None).await?;
        Ok(stream)
    }

    /// A best-effort `cancel` that never blocks the caller: the send runs on the current runtime, if there is one.
    fn cancel(&self, id: u64) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let connection = self.clone();
        runtime.spawn(async move {
            let _ = connection.inner.send(Message::Cancel { id }, None).await;
        });
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pong {
    pub seq: u64,
    pub rss: u64,
    pub at: Instant,
}

pub struct ReplyStream {
    connection: Connection,
    id: u64,
    chunks: mpsc::Receiver<Reply>,
    done: Option<oneshot::Receiver<Result<(), SidecarError>>>,
    finished: bool,
    owed: u32,
}

impl ReplyStream {
    pub async fn next(&mut self) -> Option<Result<Reply, SidecarError>> {
        if self.finished {
            return None;
        }
        if self.owed > 0 {
            let live = matches!(lock(&self.connection.inner.pending).get(&self.id), Some(Pending::Stream { .. }));
            if live
                && self.connection.inner.send(Message::Credit { id: self.id, chunks: self.owed }, None).await.is_ok()
            {
                self.owed = 0;
            }
        }
        match self.chunks.recv().await {
            Some(reply) => {
                let live = {
                    let mut pending = lock(&self.connection.inner.pending);
                    if let Some(Pending::Stream { taken, .. }) = pending.get_mut(&self.id) {
                        *taken += 1;
                        true
                    } else {
                        false
                    }
                };
                if live {
                    self.owed += 1;
                }
                Some(Ok(reply))
            }
            None => {
                self.finished = true;
                match self.done.take() {
                    Some(done) => match done.await {
                        Ok(Ok(())) => None,
                        Ok(Err(error)) => Some(Err(error)),
                        Err(_) => {
                            Some(Err(SidecarError::ProtocolBroken("a stream ended without its outcome".to_owned())))
                        }
                    },
                    None => None,
                }
            }
        }
    }
}

impl Drop for ReplyStream {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let entry = lock(&self.connection.inner.pending).remove(&self.id);
        if matches!(entry, Some(Pending::Stream { .. })) {
            self.connection.cancel(self.id);
        }
    }
}

pub struct ChunkSink {
    id: u64,
    connection: Connection,
    gate: Arc<CreditGate>,
}

impl ChunkSink {
    pub async fn send(&self, reply: Reply) -> Result<(), SidecarError> {
        self.gate.take().await?;
        let message = Message::Chunk { id: self.id, value: reply.value };
        let sent = self.connection.inner.send(message, reply.bytes.as_deref()).await;
        if matches!(sent, Err(SidecarError::TooLarge { .. })) {
            self.gate.grant(1);
        }
        sent
    }
}

struct Framed {
    bytes: Vec<u8>,
    permit: OwnedSemaphorePermit,
}

enum Pending {
    Call(oneshot::Sender<Result<Reply, SidecarError>>),
    Stream {
        chunks: mpsc::Sender<Reply>,
        done: Option<oneshot::Sender<Result<(), SidecarError>>>,
        sent: u64,
        taken: u64,
    },
}

struct Inner {
    frames: mpsc::Sender<Framed>,
    room: Arc<Semaphore>,
    budget: u32,
    window: u32,
    max_frame: usize,
    max_outgoing: usize,
    pending: Mutex<HashMap<u64, Pending>>,
    next_id: AtomicU64,
    dead: Mutex<Option<SidecarError>>,
    pong: Mutex<Option<Pong>>,
    handlers: Handlers,
    incoming: Mutex<HashMap<u64, CancellationToken>>,
    credits: Mutex<HashMap<u64, Arc<CreditGate>>>,
    slots: Arc<Semaphore>,
    refusals: Arc<Semaphore>,
}

impl Inner {
    async fn send(&self, message: Message, bytes: Option<&[u8]>) -> Result<(), SidecarError> {
        if let Some(error) = lock(&self.dead).clone() {
            return Err(error);
        }
        let frame = frame::encode(&message, bytes, self.max_frame).map_err(|error| match error {
            FrameError::TooLarge { len, max } => SidecarError::TooLarge { len, max },
            FrameError::Json(error) => {
                SidecarError::ProtocolBroken(format!("a message that would not encode: {error}"))
            }
        })?;
        let permits = frame.len().min(self.budget as usize) as u32;
        let permit = self
            .room
            .clone()
            .acquire_many_owned(permits)
            .await
            .map_err(|_| SidecarError::ProtocolBroken("the sidecar connection closed".to_owned()))?;
        self.frames.send(Framed { bytes: frame, permit }).await.map_err(|_| SidecarError::ended())
    }

    /// `pending` is taken before `dead`, always: a `fail` that takes them in the other order could miss a call
    /// inserted between the dead check and the insert, and leave that caller waiting for an answer forever.
    fn register(&self, entry: Pending) -> Result<u64, SidecarError> {
        let mut pending = lock(&self.pending);
        let dead = lock(&self.dead);
        if let Some(error) = dead.as_ref() {
            return Err(error.clone());
        }
        if pending.len() >= self.max_outgoing {
            return Err(SidecarError::Busy);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        pending.insert(id, entry);
        Ok(id)
    }

    fn remove(&self, id: u64) {
        lock(&self.pending).remove(&id);
    }

    fn fail(&self, error: SidecarError) {
        let entries = {
            let mut pending = lock(&self.pending);
            let mut dead = lock(&self.dead);
            if dead.is_some() {
                return;
            }
            *dead = Some(error.clone());
            std::mem::take(&mut *pending)
        };
        for (_, entry) in entries {
            match entry {
                Pending::Call(answer) => {
                    let _ = answer.send(Err(error.clone()));
                }
                Pending::Stream { done: Some(done), .. } => {
                    let _ = done.send(Err(error.clone()));
                }
                Pending::Stream { done: None, .. } => {}
            }
        }
        let incoming = std::mem::take(&mut *lock(&self.incoming));
        for (_, token) in incoming {
            token.cancel();
        }
        let credits = std::mem::take(&mut *lock(&self.credits));
        for (_, gate) in credits {
            gate.close();
        }
    }

    async fn receive(self: &Arc<Self>, frame: Frame) {
        let Frame { message, bytes } = frame;
        match message {
            Message::Result { id, value } => {
                let entry = lock(&self.pending).remove(&id);
                if let Some(Pending::Call(answer)) = entry {
                    let _ = answer.send(Ok(Reply { value, bytes }));
                }
            }
            Message::Error { id, error } => {
                let entry = lock(&self.pending).remove(&id);
                match entry {
                    Some(Pending::Call(answer)) => {
                        let _ = answer.send(Err(SidecarError::Remote(error)));
                    }
                    Some(Pending::Stream { done: Some(done), .. }) => {
                        let _ = done.send(Err(SidecarError::Remote(error)));
                    }
                    _ => {}
                }
            }
            Message::Chunk { id, value } => {
                let credit = self.window as u64;
                let over = {
                    let mut pending = lock(&self.pending);
                    match pending.get_mut(&id) {
                        Some(Pending::Stream { sent, taken, .. }) if *sent - *taken >= credit => true,
                        Some(Pending::Stream { chunks, sent, .. }) => {
                            *sent += 1;
                            let _ = chunks.try_send(Reply { value, bytes });
                            false
                        }
                        _ => false,
                    }
                };
                if over {
                    self.fail(SidecarError::ProtocolBroken("the sidecar sent more chunks than its credit".to_owned()));
                }
            }
            Message::End { id } => {
                let entry = lock(&self.pending).remove(&id);
                if let Some(Pending::Stream { done: Some(done), .. }) = entry {
                    let _ = done.send(Ok(()));
                }
            }
            Message::Credit { id, chunks } => {
                let gate = lock(&self.credits).get(&id).cloned();
                if let Some(gate) = gate {
                    gate.grant(chunks);
                }
            }
            Message::Cancel { id } => {
                let token = lock(&self.incoming).remove(&id);
                if let Some(token) = token {
                    token.cancel();
                }
            }
            Message::Pong { seq, rss } => {
                *lock(&self.pong) = Some(Pong { seq, rss, at: Instant::now() });
            }
            Message::Log { level, message } => {
                let level = match level {
                    LogLevel::Debug => "debug",
                    LogLevel::Info => "info",
                    LogLevel::Warn => "warn",
                    LogLevel::Error => "error",
                };
                eprintln!("sidecar [{level}]: {message}");
            }
            Message::Call { id, method, location, parent, params, stream } => {
                let request = Request { method, location, params, bytes };
                self.dispatch(id, parent, stream, request).await;
            }
            _ => {}
        }
    }

    async fn dispatch(self: &Arc<Self>, id: u64, parent: Option<u64>, stream: bool, request: Request) {
        let Ok(permit) = self.slots.clone().try_acquire_owned() else {
            let error =
                RemoteError { tag: "Busy".to_owned(), message: "too many calls in flight".to_owned(), data: None };
            let Ok(refusal) = self.refusals.clone().try_acquire_owned() else {
                let _ = self.send(Message::Error { id, error }, None).await;
                return;
            };
            let inner = Arc::clone(self);
            tokio::spawn(async move {
                let _ = inner.send(Message::Error { id, error }, None).await;
                drop(refusal);
            });
            return;
        };
        let cancel = CancellationToken::new();
        lock(&self.incoming).insert(id, cancel.clone());
        let gate = if stream {
            let fresh = Arc::new(CreditGate::new());
            lock(&self.credits).insert(id, Arc::clone(&fresh));
            Some(fresh)
        } else {
            None
        };
        let context = CallContext {
            id,
            location: request.location.clone(),
            parent,
            cancel: cancel.clone(),
            client: Client {
                connection: Connection { inner: Arc::clone(self) },
                location: request.location.clone(),
                parent: id,
            },
        };
        let inner = Arc::clone(self);
        tokio::spawn(async move {
            let _permit = permit;
            match gate {
                Some(gate) => serve_stream(Arc::clone(&inner), request, context, cancel, gate).await,
                None => serve_call(Arc::clone(&inner), request, context, cancel).await,
            }
            lock(&inner.incoming).remove(&id);
            lock(&inner.credits).remove(&id);
        });
    }
}

async fn serve_call(inner: Arc<Inner>, request: Request, context: CallContext, cancel: CancellationToken) {
    let id = context.id;
    let Some(handler) = inner.handlers.calls.get(&request.method).cloned() else {
        let error = RemoteError {
            tag: "UnknownMethod".to_owned(),
            message: format!("an unknown method {}", request.method),
            data: None,
        };
        let _ = inner.send(Message::Error { id, error }, None).await;
        return;
    };
    tokio::select! {
        () = cancel.cancelled() => {}
        outcome = handler(context, request) => match outcome {
            Ok(reply) => {
                let sent = inner.send(Message::Result { id, value: reply.value }, reply.bytes.as_deref()).await;
                if let Err(error @ SidecarError::TooLarge { .. }) = sent {
                    let error = RemoteError { tag: "TooLarge".to_owned(), message: error.to_string(), data: None };
                    let _ = inner.send(Message::Error { id, error }, None).await;
                }
            }
            Err(error) => {
                let _ = inner.send(Message::Error { id, error }, None).await;
            }
        },
    }
}

async fn serve_stream(
    inner: Arc<Inner>,
    request: Request,
    context: CallContext,
    cancel: CancellationToken,
    gate: Arc<CreditGate>,
) {
    let id = context.id;
    let Some(handler) = inner.handlers.streams.get(&request.method).cloned() else {
        let error = RemoteError {
            tag: "UnknownMethod".to_owned(),
            message: format!("an unknown method {}", request.method),
            data: None,
        };
        let _ = inner.send(Message::Error { id, error }, None).await;
        return;
    };
    let sink = ChunkSink { id, connection: Connection { inner: Arc::clone(&inner) }, gate };
    tokio::select! {
        () = cancel.cancelled() => {}
        outcome = handler(context, request, sink) => match outcome {
            Ok(()) => {
                let _ = inner.send(Message::End { id }, None).await;
            }
            Err(error) => {
                let _ = inner.send(Message::Error { id, error }, None).await;
            }
        },
    }
}

async fn read_frames(mut read: OwnedReadHalf, inner: Weak<Inner>, max_frame: usize) {
    loop {
        match frame::read(&mut read, max_frame).await {
            Ok(Some(frame)) => {
                let Some(inner) = inner.upgrade() else {
                    return;
                };
                inner.receive(frame).await;
                if lock(&inner.dead).is_some() {
                    return;
                }
            }
            Ok(None) => {
                if let Some(inner) = inner.upgrade() {
                    inner.fail(SidecarError::ended());
                }
                return;
            }
            Err(error) => {
                let reason = match error {
                    Broken::Io(_) | Broken::Truncated { .. } => SidecarError::ended(),
                    error => SidecarError::ProtocolBroken(error.to_string()),
                };
                if let Some(inner) = inner.upgrade() {
                    inner.fail(reason);
                }
                return;
            }
        }
    }
}

async fn write_frames(mut write: OwnedWriteHalf, mut queued: mpsc::Receiver<Framed>, inner: Weak<Inner>) {
    while let Some(Framed { bytes, permit }) = queued.recv().await {
        if write.write_all(&bytes).await.is_err() {
            if let Some(inner) = inner.upgrade() {
                inner.fail(SidecarError::ended());
            }
            return;
        }
        drop(permit);
    }
}

struct CallGuard {
    connection: Connection,
    id: u64,
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        let removed = lock(&self.connection.inner.pending).remove(&self.id).is_some();
        if removed {
            self.connection.cancel(self.id);
        }
    }
}

struct GateState {
    credit: u32,
    closed: bool,
}

struct CreditGate {
    state: Mutex<GateState>,
    notify: Notify,
}

impl CreditGate {
    fn new() -> Self {
        Self { state: Mutex::new(GateState { credit: 0, closed: false }), notify: Notify::new() }
    }

    fn grant(&self, chunks: u32) {
        {
            let mut state = lock(&self.state);
            state.credit = state.credit.saturating_add(chunks);
        }
        self.notify.notify_one();
    }

    fn close(&self) {
        lock(&self.state).closed = true;
        self.notify.notify_one();
    }

    async fn take(&self) -> Result<(), SidecarError> {
        loop {
            let notified = self.notify.notified();
            {
                let mut state = lock(&self.state);
                if state.closed {
                    return Err(SidecarError::ProtocolBroken("the sidecar connection closed".to_owned()));
                }
                if state.credit > 0 {
                    state.credit -= 1;
                    return Ok(());
                }
            }
            notified.await;
        }
    }
}
