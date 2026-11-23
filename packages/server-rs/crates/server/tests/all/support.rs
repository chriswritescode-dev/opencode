#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

use base64::Engine;
use rustix::process::{Pid, Signal, kill_process};

use opencode_util::temp::TempDir;

const LINE_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

pub struct Sandbox {
    dir: TempDir,
}

impl Sandbox {
    pub fn new() -> Self {
        let dir = opencode_util::temp::tempdir().expect("a temporary directory");
        for name in ["home", "tmp", "config", "data", "cache", "state"] {
            std::fs::create_dir_all(dir.path().join(name)).unwrap();
        }
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        self.dir.path()
    }
}

impl Default for Sandbox {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Process {
    child: Child,
    pid: u32,
    stdin: Option<ChildStdin>,
    stdout: Receiver<String>,
    stderr: Receiver<String>,
    stdout_thread: Option<std::thread::JoinHandle<()>>,
    stderr_thread: Option<std::thread::JoinHandle<()>>,
}

impl Process {
    pub fn spawn(sandbox: &Sandbox, args: &[&str], extra_env: &[(&str, &str)]) -> Self {
        let root = sandbox.dir();
        let mut command = Command::new(env!("CARGO_BIN_EXE_opencode-server"));
        command
            .args(args)
            .current_dir(root)
            .env_clear()
            .env("HOME", root.join("home"))
            .env("TMPDIR", root.join("tmp"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("XDG_STATE_HOME", root.join("state"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, value) in extra_env {
            command.env(name, value);
        }
        let mut child = command.spawn().unwrap_or_else(|error| panic!("spawn opencode-server {args:?}: {error}"));
        let pid = child.id();
        let stdin = child.stdin.take();
        let (stdout, stdout_thread) = pump(child.stdout.take().expect("piped stdout"));
        let (stderr, stderr_thread) = pump(child.stderr.take().expect("piped stderr"));
        Self {
            child,
            pid,
            stdin,
            stdout,
            stderr,
            stdout_thread: Some(stdout_thread),
            stderr_thread: Some(stderr_thread),
        }
    }

    pub fn line(&self) -> String {
        next_line(&self.stdout, "stdout")
    }

    pub fn error_line(&self) -> String {
        next_line(&self.stderr, "stderr")
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn close_stdin(&mut self) {
        drop(self.stdin.take());
    }

    pub fn signal(&self, signal: Signal) {
        let Some(pid) = i32::try_from(self.pid).ok().and_then(Pid::from_raw) else { return };
        let _ = kill_process(pid, signal);
    }

    pub fn wait(&mut self, timeout: Duration) -> ExitStatus {
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return status,
                Ok(None) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        panic!("opencode-server (pid {}) did not exit within {timeout:?}", self.pid);
                    }
                    std::thread::sleep(remaining.min(POLL_INTERVAL));
                }
                Err(error) => panic!("wait for opencode-server (pid {}): {error}", self.pid),
            }
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(thread) = self.stdout_thread.take() {
            let _ = thread.join();
        }
        if let Some(thread) = self.stderr_thread.take() {
            let _ = thread.join();
        }
    }
}

fn pump(pipe: impl Read + Send + 'static) -> (Receiver<String>, std::thread::JoinHandle<()>) {
    let (sender, receiver) = channel();
    let handle = std::thread::spawn(move || read_lines(pipe, sender));
    (receiver, handle)
}

fn read_lines(pipe: impl Read, sender: Sender<String>) {
    let mut reader = BufReader::new(pipe);
    let mut bytes = Vec::new();
    loop {
        bytes.clear();
        let Ok(read) = reader.read_until(b'\n', &mut bytes) else { return };
        if read == 0 {
            return;
        }
        while bytes.ends_with(b"\n") || bytes.ends_with(b"\r") {
            bytes.pop();
        }
        if sender.send(String::from_utf8_lossy(&bytes).into_owned()).is_err() {
            return;
        }
    }
}

fn next_line(receiver: &Receiver<String>, stream: &str) -> String {
    match receiver.recv_timeout(LINE_TIMEOUT) {
        Ok(line) => line,
        Err(RecvTimeoutError::Timeout) => panic!("no {stream} line from opencode-server within {LINE_TIMEOUT:?}"),
        Err(RecvTimeoutError::Disconnected) => panic!("opencode-server closed {stream} before printing a line"),
    }
}

pub fn basic_header(password: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("opencode:{password}"));
    format!("Basic {encoded}")
}

pub fn port_from_url(url: &str) -> u16 {
    let trimmed = url.trim_end_matches('/');
    let port = trimmed.rsplit(':').next().unwrap_or_default();
    port.parse().unwrap_or_else(|error| panic!("no port at the end of {url:?}: {error}"))
}

pub fn get(port: u16, path: &str, headers: &[(&str, &str)]) -> String {
    let mut stream =
        TcpStream::connect(("127.0.0.1", port)).unwrap_or_else(|error| panic!("connect to 127.0.0.1:{port}: {error}"));
    stream.set_read_timeout(Some(LINE_TIMEOUT)).unwrap();
    stream.set_write_timeout(Some(LINE_TIMEOUT)).unwrap();
    let mut request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("Connection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .unwrap_or_else(|error| panic!("read GET {path} from 127.0.0.1:{port}: {error}\n{response}"));
    response
}

pub fn json_body(response: &str) -> serde_json::Value {
    let Some((_, body)) = response.split_once("\r\n\r\n") else {
        panic!("no header/body separator in response:\n{response}");
    };
    serde_json::from_str(body).unwrap_or_else(|error| panic!("body is not JSON ({error}):\n{response}"))
}
