//! Supervising the sidecar process: one lazy start shared by concurrent callers, the heartbeat and memory check that
//! watch it, the backoff that follows a crash, and the shutdown that ends it.

use std::os::fd::AsFd;
use std::os::unix::process::ExitStatusExt;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use rustix::io::fcntl_dupfd_cloexec;
use rustix::process::{Pid, Signal, kill_process_group};
use tokio::net::UnixStream;
use tokio::sync::watch;
use tokio::time::MissedTickBehavior;

use opencode_util::lock;

use crate::config::{Config, Limits};
use crate::connection::{Connection, Handlers, Reply, ReplyStream, Request};
use crate::error::SidecarError;
use crate::spawn::{Spawned, spawn};

/// The supervisor for one sidecar process.
///
/// The first call or stream starts it, and concurrent first callers share that one start. A crash is followed by a
/// growing backoff, and too many crashes in a row disable the sidecar until a new supervisor is made.
#[derive(Clone)]
pub struct Sidecar(Arc<Inner>);

impl Sidecar {
    /// A supervisor that starts nothing until its first call or stream.
    pub fn new(config: Config, handlers: Handlers) -> Sidecar {
        let (state, _) = watch::channel(State::Idle);
        Sidecar(Arc::new(Inner {
            config,
            handlers,
            slot: tokio::sync::Mutex::new(Slot::Idle),
            state,
            generation: AtomicU64::new(0),
            pid: AtomicU32::new(0),
            current: std::sync::Mutex::new(None),
            shutting_down: AtomicBool::new(false),
        }))
    }

    /// The supervisor's state, from now on and at every change.
    pub fn state(&self) -> watch::Receiver<State> {
        self.0.state.subscribe()
    }

    /// The running sidecar's process id, for tests and diagnostics.
    #[doc(hidden)]
    pub fn pid(&self) -> Option<u32> {
        let pid = self.0.pid.load(Ordering::SeqCst);
        (pid != 0).then_some(pid)
    }

    /// Calls a method on the host, starting the sidecar first when nothing is running.
    pub async fn call(&self, request: Request) -> Result<Reply, SidecarError> {
        let live = self.0.live().await?;
        match live.connection.call(request).await {
            Ok(reply) => Ok(reply),
            Err(error) => Err(translate(live.exit.clone(), error).await),
        }
    }

    /// Opens a stream from the host, starting the sidecar first when nothing is running.
    pub async fn stream(&self, request: Request) -> Result<Stream, SidecarError> {
        let live = self.0.live().await?;
        match live.connection.stream(request).await {
            Ok(replies) => Ok(Stream { replies, exit: live.exit.clone() }),
            Err(error) => Err(translate(live.exit.clone(), error).await),
        }
    }

    /// Stops the sidecar for good; later calls and streams fail with [`SidecarError::ShuttingDown`].
    pub async fn shutdown(&self) {
        self.0.shutting_down.store(true, Ordering::SeqCst);
        let live = loop {
            let mut progress = {
                let mut slot = self.0.slot.lock().await;
                match &*slot {
                    Slot::Idle | Slot::Backoff { .. } | Slot::Disabled { .. } => {
                        *slot = Slot::ShuttingDown;
                        return;
                    }
                    Slot::ShuttingDown => return,
                    Slot::Starting { progress, .. } => progress.clone(),
                    Slot::Ready(live) => {
                        let live = Arc::clone(live);
                        *slot = Slot::ShuttingDown;
                        self.0.pid.store(0, Ordering::SeqCst);
                        *lock(&self.0.current) = None;
                        break live;
                    }
                }
            };
            if wait_start(&mut progress).await.is_none() {
                let mut slot = self.0.slot.lock().await;
                if matches!(*slot, Slot::Starting { .. }) {
                    *slot = Slot::ShuttingDown;
                }
                return;
            }
        };

        // The host exits on the end of its input and runs its own `killAll`; the group kill is the fallback for a host
        // that does not.
        let _ = live.socket.shutdown(std::net::Shutdown::Write);
        if exited(&live.exit, self.0.config.limits.grace).await {
            return;
        }
        kill_group(live.pid);
        exited(&live.exit, self.0.config.limits.grace).await;
    }
}

/// The supervisor's state, as [`Sidecar::state`] reports it.
#[derive(Clone, Debug)]
pub enum State {
    /// No process is running and none is starting.
    Idle,
    /// Generation `generation` is running as `pid` and answering.
    Ready { generation: u64, pid: u32 },
    /// Generation `generation` stopped for `reason`.
    Exited { generation: u64, reason: SidecarError },
    /// The sidecar crashed too often and is not started again.
    Disabled,
}

/// A stream of replies from one sidecar generation.
pub struct Stream {
    replies: ReplyStream,
    exit: watch::Receiver<Option<SidecarError>>,
}

impl Stream {
    /// The next reply, `None` once the stream ended; an error names why the sidecar stopped.
    pub async fn next(&mut self) -> Option<Result<Reply, SidecarError>> {
        match self.replies.next().await {
            Some(Err(error)) => Some(Err(translate(self.exit.clone(), error).await)),
            other => other,
        }
    }
}

type Progress = watch::Receiver<Option<Result<Arc<Live>, SidecarError>>>;

struct Live {
    generation: u64,
    crashes: usize,
    pid: u32,
    connection: Connection,
    socket: std::os::unix::net::UnixStream,
    exit: watch::Receiver<Option<SidecarError>>,
}

enum Slot {
    Idle,
    Starting { progress: Progress, crashes: usize },
    Ready(Arc<Live>),
    Backoff { until: Instant, crashes: usize },
    Disabled { crashes: usize },
    ShuttingDown,
}

struct Inner {
    config: Config,
    handlers: Handlers,
    slot: tokio::sync::Mutex<Slot>,
    state: watch::Sender<State>,
    generation: AtomicU64,
    pid: AtomicU32,
    current: std::sync::Mutex<Option<Arc<Live>>>,
    shutting_down: AtomicBool,
}

impl Inner {
    /// The running generation, starting one when none is running and the backoff allows it. The slot is held only to
    /// decide the transition and to record the outcome, never while a process is spawned.
    async fn live(self: &Arc<Self>) -> Result<Arc<Live>, SidecarError> {
        let mut progress = {
            let mut slot = self.slot.lock().await;
            match &*slot {
                Slot::Ready(live) => return Ok(Arc::clone(live)),
                Slot::Starting { progress, .. } => progress.clone(),
                Slot::Disabled { crashes } => return Err(SidecarError::Disabled { crashes: *crashes }),
                Slot::ShuttingDown => return Err(SidecarError::ShuttingDown),
                Slot::Idle => begin(self, &mut slot, 0),
                Slot::Backoff { until, crashes } => {
                    let wait = *until;
                    let crashes = *crashes;
                    let now = Instant::now();
                    if wait > now {
                        return Err(SidecarError::Unavailable { retry_in: wait - now });
                    }
                    begin(self, &mut slot, crashes)
                }
            }
        };
        let Some(result) = wait_start(&mut progress).await else {
            let mut slot = self.slot.lock().await;
            if matches!(*slot, Slot::Starting { .. }) {
                *slot = Slot::Idle;
            }
            return Err(SidecarError::Spawn("the sidecar start ended".to_owned()));
        };
        result
    }

    /// Records the end of generation `generation`: a backoff with the next crash count, or the disabled state once the
    /// crash limit is reached. A generation the slot no longer holds is stale and changes nothing.
    async fn record_exit(&self, generation: u64, crashes: usize, started: Instant, reason: SidecarError) {
        let mut slot = self.slot.lock().await;
        if matches!(*slot, Slot::ShuttingDown) {
            self.state.send_replace(State::Exited { generation, reason });
            return;
        }
        if !matches!(&*slot, Slot::Ready(live) if live.generation == generation) {
            return;
        }
        let crashes = if started.elapsed() >= self.config.limits.crash_window { 1 } else { crashes + 1 };
        let delay = backoff(&self.config.limits, crashes);
        self.pid.store(0, Ordering::SeqCst);
        {
            let mut current = lock(&self.current);
            if current.as_ref().is_some_and(|live| live.generation == generation) {
                *current = None;
            }
        }
        self.state.send_replace(State::Exited { generation, reason });
        if crashes >= self.config.limits.crash_limit {
            *slot = Slot::Disabled { crashes };
            self.state.send_replace(State::Disabled);
        } else {
            *slot = Slot::Backoff { until: Instant::now() + delay, crashes };
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        let Some(live) = lock(&self.current).take() else {
            return;
        };
        let _ = live.socket.shutdown(std::net::Shutdown::Write);
        kill_group(live.pid);
    }
}

/// Takes the [`Slot::Idle`] or expired [`Slot::Backoff`] transition: the slot becomes [`Slot::Starting`] and the start
/// runs in its own task, so the callers that arrive meanwhile wait on the same progress receiver.
fn begin(inner: &Arc<Inner>, slot: &mut Slot, crashes: usize) -> Progress {
    let (sender, progress) = watch::channel(None::<Result<Arc<Live>, SidecarError>>);
    *slot = Slot::Starting { progress: progress.clone(), crashes };
    let weak = Arc::downgrade(inner);
    tokio::spawn(async move {
        sender.send_replace(Some(start(weak, crashes).await));
    });
    progress
}

async fn start(inner: Weak<Inner>, crashes: usize) -> Result<Arc<Live>, SidecarError> {
    let Some(inner) = inner.upgrade() else {
        return Err(SidecarError::ShuttingDown);
    };
    let Some(runtime) = inner.config.runtime().await else {
        let mut slot = inner.slot.lock().await;
        if matches!(*slot, Slot::Starting { .. }) {
            *slot = Slot::Idle;
        }
        return Err(SidecarError::BunNotFound);
    };
    if inner.shutting_down.load(Ordering::SeqCst) {
        *inner.slot.lock().await = Slot::ShuttingDown;
        return Err(SidecarError::ShuttingDown);
    }
    let spawned = match spawn(&inner.config, &runtime).await {
        Ok(spawned) => spawned,
        Err(error) => return Err(start_failed(&inner, crashes, io_error(error)).await),
    };

    let prepared = fcntl_dupfd_cloexec(spawned.stream.as_fd(), 0).map_err(std::io::Error::from).and_then(|copy| {
        let stream = std::os::unix::net::UnixStream::from(copy);
        let socket = stream.try_clone()?;
        let stream = UnixStream::from_std(stream)?;
        Ok((stream, socket))
    });
    let (stream, socket) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => return Err(start_failed(&inner, crashes, io_error(error)).await),
    };
    let connection = Connection::start(stream, &inner.config.limits, inner.handlers.clone());

    let generation = inner.generation.fetch_add(1, Ordering::SeqCst) + 1;
    let (exit, receiver) = watch::channel(None::<SidecarError>);
    let live = Arc::new(Live {
        generation,
        crashes,
        pid: spawned.pid,
        connection: connection.clone(),
        socket,
        exit: receiver,
    });
    tokio::spawn(monitor(
        Arc::downgrade(&inner),
        generation,
        live.crashes,
        spawned,
        connection,
        exit,
        inner.config.limits.clone(),
    ));

    let mut slot = inner.slot.lock().await;
    if !matches!(&*slot, Slot::Starting { crashes: starting, .. } if *starting == crashes) {
        drop(slot);
        kill_group(live.pid);
        return Err(SidecarError::ShuttingDown);
    }
    *slot = Slot::Ready(Arc::clone(&live));
    inner.pid.store(live.pid, Ordering::SeqCst);
    *lock(&inner.current) = Some(Arc::clone(&live));
    inner.state.send_replace(State::Ready { generation, pid: live.pid });
    Ok(live)
}

/// Watches one process: the heartbeat that proves it still answers, its memory ceiling, and the wait for its exit.
///
/// It owns the [`Spawned`] value, so the child, the private directory and the stderr ring live as long as the process
/// does, and it always sends the exit reason before [`Inner::record_exit`] publishes the state that follows it.
async fn monitor(
    inner: Weak<Inner>,
    generation: u64,
    crashes: usize,
    mut spawned: Spawned,
    connection: Connection,
    exit: watch::Sender<Option<SidecarError>>,
    limits: Limits,
) {
    let started = Instant::now();
    let pid = spawned.pid;
    let mut ticker = tokio::time::interval(limits.heartbeat.max(Duration::from_millis(1)));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut seen = started;
    let mut seq = 0u64;
    let mut ended: Option<SidecarError> = None;
    loop {
        tokio::select! {
            status = spawned.child.wait() => {
                let reason = match ended {
                    Some(reason) => reason,
                    None if shutting_down(&inner) => SidecarError::ShuttingDown,
                    None => exited_with(status, spawned.stderr()),
                };
                exit.send_replace(Some(reason.clone()));
                if let Some(inner) = inner.upgrade() {
                    inner.record_exit(generation, crashes, started, reason).await;
                }
                return;
            }
            _ = ticker.tick() => {
                if ended.is_some() {
                    continue;
                }
                let Some(inner) = inner.upgrade() else {
                    return;
                };
                if inner.shutting_down.load(Ordering::SeqCst) {
                    continue;
                }
                if let Some(pong) = connection.pong() {
                    seen = seen.max(pong.at);
                    if pong.rss > limits.memory {
                        ended = Some(SidecarError::MemoryExceeded { rss: pong.rss });
                    }
                }
                if ended.is_none() && seen.elapsed() >= limits.liveness {
                    ended = Some(SidecarError::Unresponsive);
                }
                if ended.is_some() {
                    kill_group(pid);
                    continue;
                }
                seq += 1;
                if tokio::time::timeout(limits.liveness, connection.ping(seq)).await.is_err() {
                    ended = Some(SidecarError::Unresponsive);
                    kill_group(pid);
                }
            }
        }
    }
}

/// The connection reports its own end as it closes, which can happen before the monitor has reaped the child. The exit
/// watch carries the process's real reason, so a call that fails that way waits briefly for it and reports it instead.
async fn translate(mut exit: watch::Receiver<Option<SidecarError>>, error: SidecarError) -> SidecarError {
    if !error.is_ended() {
        return error;
    }
    let Ok(Ok(reason)) = tokio::time::timeout(Duration::from_secs(10), exit.wait_for(|reason| reason.is_some())).await
    else {
        return error;
    };
    match reason.as_ref() {
        Some(reason) => reason.clone(),
        None => error,
    }
}

/// The start task's result, or `None` when its channel closed without one.
///
/// The wait lives in its own function because a `watch::Ref` is not `Send`: kept in the caller's state machine, it
/// would make that future un-sendable even though it is never held across an await.
async fn wait_start(progress: &mut Progress) -> Option<Result<Arc<Live>, SidecarError>> {
    match progress.wait_for(|value| value.is_some()).await {
        Ok(value) => Some(value.as_ref().expect("the sidecar start published a result").clone()),
        Err(_) => None,
    }
}

async fn exited(exit: &watch::Receiver<Option<SidecarError>>, within: Duration) -> bool {
    let mut exit = exit.clone();
    matches!(tokio::time::timeout(within, exit.wait_for(|reason| reason.is_some())).await, Ok(Ok(_)))
}

fn shutting_down(inner: &Weak<Inner>) -> bool {
    inner.upgrade().is_some_and(|inner| inner.shutting_down.load(Ordering::SeqCst))
}

fn exited_with(status: std::io::Result<std::process::ExitStatus>, mut stderr: Vec<String>) -> SidecarError {
    match status {
        Ok(status) => SidecarError::Exited { status: status.code(), signal: status.signal(), stderr },
        Err(error) => {
            stderr.push(error.to_string());
            SidecarError::Exited { status: None, signal: None, stderr }
        }
    }
}

fn backoff(limits: &Limits, crashes: usize) -> Duration {
    let factor = 1u32 << crashes.saturating_sub(1).min(31);
    limits.backoff_initial.saturating_mul(factor).min(limits.backoff_max)
}

/// Records a start that never reached a running process as a crash, so a failing start backs off instead of retrying
/// in a hot loop, and returns `error`.
async fn start_failed(inner: &Inner, crashes: usize, error: SidecarError) -> SidecarError {
    let crashes = crashes + 1;
    let delay = backoff(&inner.config.limits, crashes);
    let disabled = crashes >= inner.config.limits.crash_limit;
    let mut slot = inner.slot.lock().await;
    if matches!(*slot, Slot::Starting { .. }) {
        *slot = if disabled {
            Slot::Disabled { crashes }
        } else {
            Slot::Backoff { until: Instant::now() + delay, crashes }
        };
        if disabled {
            inner.state.send_replace(State::Disabled);
        }
    }
    error
}

fn io_error(error: std::io::Error) -> SidecarError {
    SidecarError::Spawn(error.to_string())
}

fn kill_group(pid: u32) {
    if let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) {
        let _ = kill_process_group(pid, Signal::KILL);
    }
}
