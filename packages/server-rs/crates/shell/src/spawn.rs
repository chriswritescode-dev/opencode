use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

pub const DRAIN_AFTER_EXIT: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Default)]
pub struct Tracker {
    pub size: u64,
    pub newlines: u64,
    pub line_starts: [u64; 2],
}

impl Tracker {
    pub fn record(&mut self, chunk: &[u8]) {
        for (index, byte) in chunk.iter().enumerate() {
            if *byte != b'\n' {
                continue;
            }
            self.newlines += 1;
            self.line_starts[0] = self.line_starts[1];
            self.line_starts[1] = self.size + index as u64 + 1;
        }
        self.size += chunk.len() as u64;
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Exit {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

pub struct Running {
    pub pid: u32,
    pub tracker: Arc<Mutex<Tracker>>,
    child: Child,
    stdout: JoinHandle<()>,
    stderr: JoinHandle<()>,
    file: Arc<Mutex<std::fs::File>>,
}

pub struct Spawn<'a> {
    pub program: &'a str,
    pub args: &'a [String],
    pub cwd: &'a str,
    pub env: &'a [(String, String)],
    pub file: &'a Path,
}

impl Running {
    pub fn supervise(self, on_exit: impl FnOnce(Exit) + Send + 'static) {
        let Running { mut child, mut stdout, mut stderr, file, .. } = self;
        tokio::spawn(async move {
            let exit = match child.wait().await {
                Ok(status) => exit_of(status),
                Err(_) => Exit::default(),
            };
            let drained = tokio::time::timeout(DRAIN_AFTER_EXIT, async {
                let _ = (&mut stdout).await;
                let _ = (&mut stderr).await;
            })
            .await;
            if drained.is_err() {
                stdout.abort();
                stderr.abort();
                let _ = stdout.await;
                let _ = stderr.await;
            }
            if let Ok(mut file) = file.lock() {
                let _ = std::io::Write::flush(&mut *file);
            }
            on_exit(exit);
        });
    }
}

pub fn start(spawn: Spawn<'_>) -> std::io::Result<Running> {
    let file = std::fs::File::create(spawn.file)?;
    let mut command = Command::new(spawn.program);
    command
        .args(spawn.args)
        .current_dir(spawn.cwd)
        .env_clear()
        .envs(spawn.env.iter().map(|(name, value)| (name.as_str(), value.as_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(command.as_std_mut(), 0);
    let mut child = command.spawn()?;
    let pid = child.id().ok_or_else(|| std::io::Error::other("the shell command has no pid"))?;
    let stdout = child.stdout.take().ok_or_else(|| std::io::Error::other("the shell command has no stdout"))?;
    let stderr = child.stderr.take().ok_or_else(|| std::io::Error::other("the shell command has no stderr"))?;
    let file = Arc::new(Mutex::new(file));
    let tracker = Arc::new(Mutex::new(Tracker::default()));
    Ok(Running {
        pid,
        tracker: tracker.clone(),
        child,
        stdout: tokio::spawn(pump(stdout, file.clone(), tracker.clone())),
        stderr: tokio::spawn(pump(stderr, file.clone(), tracker)),
        file,
    })
}

async fn pump<R>(mut reader: R, file: Arc<Mutex<std::fs::File>>, tracker: Arc<Mutex<Tracker>>)
where
    R: AsyncRead + Unpin,
{
    let mut buffer = vec![0u8; 8192];
    loop {
        let count = match reader.read(&mut buffer).await {
            Ok(0) => return,
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        };
        let mut tracker = tracker.lock().expect("the shell output tracker lock is never poisoned");
        if let Ok(mut file) = file.lock() {
            let _ = std::io::Write::write_all(&mut *file, &buffer[..count]);
            let _ = std::io::Write::flush(&mut *file);
        }
        tracker.record(&buffer[..count]);
    }
}

pub struct ProcessGroup {
    pid: u32,
}

impl ProcessGroup {
    pub fn new(pid: u32) -> Self {
        Self { pid }
    }

    #[cfg(unix)]
    fn signal(&self, signal: rustix::process::Signal) {
        let Some(pid) = i32::try_from(self.pid).ok().and_then(rustix::process::Pid::from_raw) else {
            return;
        };
        let _ = rustix::process::kill_process_group(pid, signal);
    }

    pub fn terminate(&self) {
        #[cfg(unix)]
        self.signal(rustix::process::Signal::TERM);
        #[cfg(windows)]
        self.force();
    }

    pub fn force(&self) {
        #[cfg(unix)]
        self.signal(rustix::process::Signal::KILL);
        #[cfg(windows)]
        {
            let _ = std::process::Command::new("taskkill")
                .arg("/pid")
                .arg(self.pid.to_string())
                .arg("/T")
                .arg("/F")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

#[cfg(unix)]
fn exit_of(status: std::process::ExitStatus) -> Exit {
    use std::os::unix::process::ExitStatusExt;
    Exit { code: status.code(), signal: status.signal() }
}

#[cfg(not(unix))]
fn exit_of(status: std::process::ExitStatus) -> Exit {
    Exit { code: status.code(), signal: None }
}

pub fn signal_name(raw: i32) -> String {
    #[cfg(unix)]
    {
        use rustix::process::Signal;
        let name = match Signal::from_named_raw(raw) {
            Some(Signal::HUP) => "HUP",
            Some(Signal::INT) => "INT",
            Some(Signal::QUIT) => "QUIT",
            Some(Signal::ILL) => "ILL",
            Some(Signal::TRAP) => "TRAP",
            Some(Signal::ABORT) => "ABRT",
            Some(Signal::BUS) => "BUS",
            Some(Signal::FPE) => "FPE",
            Some(Signal::KILL) => "KILL",
            Some(Signal::USR1) => "USR1",
            Some(Signal::SEGV) => "SEGV",
            Some(Signal::USR2) => "USR2",
            Some(Signal::PIPE) => "PIPE",
            Some(Signal::ALARM) => "ALRM",
            Some(Signal::TERM) => "TERM",
            Some(Signal::CHILD) => "CHLD",
            Some(Signal::CONT) => "CONT",
            Some(Signal::STOP) => "STOP",
            Some(Signal::TSTP) => "TSTP",
            Some(Signal::TTIN) => "TTIN",
            Some(Signal::TTOU) => "TTOU",
            Some(Signal::URG) => "URG",
            Some(Signal::XCPU) => "XCPU",
            Some(Signal::XFSZ) => "XFSZ",
            Some(Signal::VTALARM) => "VTALRM",
            Some(Signal::PROF) => "PROF",
            Some(Signal::WINCH) => "WINCH",
            Some(Signal::IO) => "IO",
            Some(Signal::POWER) => "PWR",
            Some(Signal::SYS) => "SYS",
            _ => return format!("SIG{raw}"),
        };
        format!("SIG{name}")
    }
    #[cfg(not(unix))]
    {
        format!("SIG{raw}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_newlines_and_the_last_two_line_starts_across_chunks() {
        let mut tracker = Tracker::default();
        tracker.record(b"one\ntw");
        tracker.record(b"o\nthree");
        assert_eq!(tracker.size, 13);
        assert_eq!(tracker.newlines, 2);
        assert_eq!(tracker.line_starts, [4, 8]);
    }

    #[test]
    fn names_signals_like_node() {
        assert_eq!(signal_name(15), "SIGTERM");
        assert_eq!(signal_name(9), "SIGKILL");
        assert_eq!(signal_name(14), "SIGALRM");
    }
}
