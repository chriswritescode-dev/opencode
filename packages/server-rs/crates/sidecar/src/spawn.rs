//! Starting the sidecar: a private directory holding the host script and its `bunfig.toml`, a socket on fd 0, an
//! allowlisted environment, and the handshake that proves the child speaks this protocol.

use std::collections::VecDeque;
use std::io;
use std::os::fd::OwnedFd;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use opencode_util::lock;
use opencode_util::temp::TempDir;

use crate::bun::Runtime;
use crate::config::{Config, inherited};
use crate::error::SidecarError;
use crate::frame::{self, Frame};
use crate::protocol::{MAGIC, Message, VERSION};

/// The JS host's bundled source, written to the private directory and run by Bun.
const HOST: &str = include_str!("../host/host.js");

/// The host's own `bunfig.toml`: Bun installs nothing the host imports.
const BUNFIG: &str = "[install]\nauto = \"disable\"\n";

const LINE_LIMIT: usize = 8 << 10;
const STDERR_LINES: usize = 20;

pub struct Spawned {
    pub child: Child,
    /// The server's end of the socket on the child's fd 0.
    pub stream: UnixStream,
    pub pid: u32,
    stderr: Arc<Mutex<VecDeque<String>>>,
    readers: Vec<JoinHandle<()>>,
    _dir: TempDir,
}

impl Spawned {
    /// The last [`STDERR_LINES`] lines the host wrote to its stdout and stderr.
    pub fn stderr(&self) -> Vec<String> {
        lock(&self.stderr).iter().cloned().collect()
    }

    pub async fn drained_stderr(&mut self) -> Vec<String> {
        for reader in &mut self.readers {
            let _ = timeout(Duration::from_millis(500), reader).await;
        }
        self.stderr()
    }
}

impl Drop for Spawned {
    fn drop(&mut self) {
        for reader in &self.readers {
            reader.abort();
        }
    }
}

/// Starts `runtime` in a private directory under `config.tmp` and completes the handshake.
pub async fn spawn(config: &Config, runtime: &Runtime) -> Result<Spawned, SidecarError> {
    let dir = TempDir::new_in(&config.tmp).map_err(spawn_failed)?;
    std::fs::write(dir.path().join("host.js"), HOST).map_err(spawn_failed)?;
    std::fs::write(dir.path().join("bunfig.toml"), BUNFIG).map_err(spawn_failed)?;

    let (ours, theirs) = std::os::unix::net::UnixStream::pair().map_err(spawn_failed)?;
    ours.set_nonblocking(true).map_err(spawn_failed)?;

    let mut command = Command::new(&runtime.bun);
    command
        .current_dir(dir.path())
        .env_clear()
        .stdin(Stdio::from(OwnedFd::from(theirs)))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (name, value) in &config.vars {
        if inherited(name) {
            command.env(name, value);
        }
    }
    if runtime.opencode {
        command.env("BUN_BE_BUN", "1");
    }
    command
        .arg("--no-env-file")
        .arg("--no-install")
        .arg(format!("--config={}", dir.path().join("bunfig.toml").display()))
        .arg(dir.path().join("host.js"));
    std::os::unix::process::CommandExt::process_group(command.as_std_mut(), 0);

    let mut child = command.spawn().map_err(spawn_failed)?;
    let stdout = child.stdout.take().ok_or_else(|| SidecarError::Spawn("the sidecar has no stdout".to_owned()))?;
    let stderr_pipe = child.stderr.take().ok_or_else(|| SidecarError::Spawn("the sidecar has no stderr".to_owned()))?;
    let stderr = Arc::new(Mutex::new(VecDeque::new()));
    let readers = vec![
        tokio::spawn(forward(stdout, Arc::clone(&stderr))),
        tokio::spawn(forward(stderr_pipe, Arc::clone(&stderr))),
    ];

    let stream = UnixStream::from_std(ours).map_err(spawn_failed)?;
    let Some(pid) = child.id() else {
        return Err(SidecarError::Handshake("the sidecar exited before the handshake".to_owned()));
    };
    let mut spawned = Spawned { child, stream, pid, stderr, readers, _dir: dir };
    match timeout(config.limits.handshake, handshake(&mut spawned.stream, config)).await {
        Ok(Ok(())) => Ok(spawned),
        Ok(Err(error)) => Err(report(&mut spawned, error.to_string()).await),
        Err(_elapsed) => {
            let message = format!("the sidecar was not ready within {:?}", config.limits.handshake);
            Err(report(&mut spawned, message).await)
        }
    }
}

async fn handshake(stream: &mut UnixStream, config: &Config) -> io::Result<()> {
    let hello = Message::Hello {
        version: VERSION,
        max_frame: u32::try_from(config.limits.max_frame).unwrap_or(u32::MAX),
        window: config.limits.window,
        heartbeat_ms: u32::try_from(config.limits.heartbeat.as_millis()).unwrap_or(u32::MAX),
        test: config.test_methods,
    };
    let bytes = frame::encode(&hello, None, config.limits.max_frame).map_err(io::Error::other)?;
    stream.write_all(&bytes).await?;
    stream.flush().await?;

    loop {
        match frame::read(stream, config.limits.max_frame).await {
            Ok(Some(Frame { message: Message::Ready { magic, version, .. }, .. })) => {
                if magic == MAGIC && version == VERSION {
                    return Ok(());
                }
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("the sidecar is ready as {magic} v{version}"),
                ));
            }
            Ok(Some(_)) => continue,
            Ok(None) => {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the sidecar ended before it was ready"));
            }
            Err(error) => return Err(io::Error::other(error)),
        }
    }
}

/// Kills the process, lets the line readers drain what it wrote, and adds the last lines, which say why it failed.
async fn report(spawned: &mut Spawned, message: String) -> SidecarError {
    let _ = spawned.child.kill().await;
    let lines = spawned.drained_stderr().await;
    if lines.is_empty() {
        return SidecarError::Handshake(message);
    }
    SidecarError::Handshake(format!("{message}; the sidecar's last stderr:\n{}", lines.join("\n")))
}

fn spawn_failed(error: io::Error) -> SidecarError {
    SidecarError::Spawn(error.to_string())
}

/// Writes every line the host prints to the server's stderr and keeps the last [`STDERR_LINES`] of them.
async fn forward<R>(pipe: R, ring: Arc<Mutex<VecDeque<String>>>)
where
    R: AsyncRead + Unpin,
{
    let mut reader = BufReader::with_capacity(LINE_LIMIT, pipe);
    let mut line = Vec::new();
    loop {
        match read_line(&mut reader, &mut line).await {
            Ok(0) => break,
            Ok(_) => {
                let text = String::from_utf8_lossy(&line);
                let text = text.trim_end_matches(['\n', '\r']);
                eprintln!("sidecar: {text}");
                let mut ring = lock(&ring);
                if ring.len() == STDERR_LINES {
                    ring.pop_front();
                }
                ring.push_back(text.to_owned());
            }
            Err(_) => break,
        }
    }
}

/// One line, without its newline, kept to [`LINE_LIMIT`]: the rest of an over-long line is consumed but not stored.
async fn read_line<R>(reader: &mut R, line: &mut Vec<u8>) -> io::Result<usize>
where
    R: AsyncBufRead + Unpin,
{
    line.clear();
    let mut total = 0;
    loop {
        let (consumed, newline) = {
            let available = reader.fill_buf().await?;
            if available.is_empty() {
                return Ok(total);
            }
            let newline = available.iter().position(|byte| *byte == b'\n');
            let chunk = &available[..newline.map_or(available.len(), |at| at + 1)];
            let room = LINE_LIMIT.saturating_sub(line.len());
            let keep = chunk.len().min(room);
            line.extend_from_slice(&chunk[..keep]);
            (chunk.len(), newline.is_some())
        };
        total += consumed;
        reader.consume(consumed);
        if newline {
            return Ok(total);
        }
    }
}
