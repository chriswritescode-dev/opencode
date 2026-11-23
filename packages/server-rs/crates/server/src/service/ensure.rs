use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use opencode_local::registration::{self, Files, Registration};

use crate::env::ServerEnv;
use crate::probe::{self, Probe, State};

const POLL_INTERVAL: Duration = Duration::from_millis(25);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const SPAWN_DELAY: Duration = Duration::from_secs(5);
const MAX_SPAWN_DELAY: Duration = Duration::from_secs(30);
const TIMEOUT: Duration = Duration::from_secs(120);
const STDERR_LIMIT: usize = 8 * 1024;
const UNRESPONSIVE: u32 = 3;

pub async fn ensure(files: &Files, env: &ServerEnv) -> Result<String, String> {
    let mut pool = Pool::new();
    let mut timeouts: Option<(Registration, u32)> = None;
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let registration = registration::read(&files.file);
        let probe = match &registration {
            Some(registration) => probe::probe(registration, REQUEST_TIMEOUT).await,
            None => Probe::Missing,
        };
        if probe == Probe::TimedOut {
            if let Some(registration) = &registration {
                let count = match &timeouts {
                    Some((previous, count)) if previous.same(registration) => count + 1,
                    _ => 1,
                };
                timeouts = Some((registration.clone(), count));
                if count >= UNRESPONSIVE {
                    super::stop::terminate(&files.file, registration).await;
                    pool.evict(registration.pid);
                    pool.recruit_now();
                    timeouts = None;
                }
            }
        } else {
            timeouts = None;
        }
        let registered = registration.is_some();
        match probe {
            Probe::Answered(State::Waiting) => pool.service_answered(),
            Probe::Answered(State::Ready) => {
                pool.service_answered();
                if let Some(registration) = registration {
                    return Ok(registration.url);
                }
            }
            Probe::Answered(State::Failed) => {
                pool.service_answered();
                return Err("Background service failed to start".to_owned());
            }
            Probe::Incompatible => {
                pool.service_answered();
                return Err(
                    "Background service uses an incompatible health protocol. Update this client or explicitly restart the service."
                        .to_owned(),
                );
            }
            Probe::Missing | Probe::TimedOut => {}
        }
        if !matches!(probe, Probe::Answered(_)) {
            if let Some(failure) = pool.reap() {
                return Err(failure);
            }
            if pool.should_recruit(registered) {
                pool.add(spawn(env)?);
            }
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    Err(pool.failure().unwrap_or_else(|| "Timed out waiting for the background service to start".to_owned()))
}

fn spawn(env: &ServerEnv) -> Result<Contender, String> {
    let exe = std::env::current_exe().map_err(|error| format!("Failed to start server: {error}"))?;
    let mut command = Command::new(exe);
    command.args(["serve", "--service"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    command.env_clear();
    for (name, value) in env.vars() {
        command.env(name, value);
    }
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut command, 0x0000_0200 | 0x0800_0000);
    let mut child = command.spawn().map_err(|error| format!("Failed to start server: {error}"))?;
    let mut stderr = child.stderr.take().ok_or_else(|| "Failed to start server: stderr is not piped".to_owned())?;
    let tail = Arc::new(Mutex::new(Vec::new()));
    let buffer = Arc::clone(&tail);
    std::thread::spawn(move || {
        let mut chunk = [0; 4096];
        while let Ok(read) = stderr.read(&mut chunk) {
            if read == 0 {
                break;
            }
            let mut tail = buffer.lock().unwrap();
            tail.extend_from_slice(&chunk[..read]);
            let excess = tail.len().saturating_sub(STDERR_LIMIT);
            tail.drain(..excess);
        }
    });
    Ok(Contender { pid: child.id(), child: Mutex::new(child), status: Mutex::new(None), stderr: tail })
}

struct Contender {
    pid: u32,
    child: Mutex<Child>,
    status: Mutex<Option<ExitStatus>>,
    stderr: Arc<Mutex<Vec<u8>>>,
}

impl Contender {
    fn pid(&self) -> u32 {
        self.pid
    }

    fn status(&self) -> Option<ExitStatus> {
        if let Some(status) = *self.status.lock().unwrap() {
            return Some(status);
        }
        let status = self.child.lock().unwrap().try_wait().ok().flatten()?;
        *self.status.lock().unwrap() = Some(status);
        Some(status)
    }

    fn finished(&self) -> bool {
        self.status().is_some()
    }

    fn failure(&self) -> Option<String> {
        let status = self.status()?;
        let message = match (status.code(), signal(status)) {
            (Some(0), _) => return None,
            (Some(code), _) => format!("Server process exited with code {code}"),
            (None, Some(signal)) => format!("Server process terminated by {signal}"),
            (None, None) => return None,
        };
        let stderr = self.stderr();
        Some(if stderr.is_empty() { message } else { format!("{message}\n{stderr}") })
    }

    fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.stderr.lock().unwrap()).trim().to_owned()
    }
}

#[cfg(unix)]
fn signal(status: ExitStatus) -> Option<String> {
    let number = std::os::unix::process::ExitStatusExt::signal(&status)?;
    Some(
        match number {
            1 => "SIGHUP",
            2 => "SIGINT",
            3 => "SIGQUIT",
            4 => "SIGILL",
            6 => "SIGABRT",
            8 => "SIGFPE",
            9 => "SIGKILL",
            11 => "SIGSEGV",
            13 => "SIGPIPE",
            14 => "SIGALRM",
            15 => "SIGTERM",
            _ => return Some(format!("signal {number}")),
        }
        .to_owned(),
    )
}

#[cfg(not(unix))]
fn signal(_: ExitStatus) -> Option<String> {
    None
}

struct Pool {
    contenders: Vec<Contender>,
    failure: Option<String>,
    spawn_delay: Duration,
    last_spawn: Option<Instant>,
}

impl Pool {
    fn new() -> Self {
        Self { contenders: Vec::new(), failure: None, spawn_delay: SPAWN_DELAY, last_spawn: None }
    }

    fn failure(&self) -> Option<String> {
        self.failure.clone()
    }

    fn service_answered(&mut self) {
        self.spawn_delay = SPAWN_DELAY;
    }

    fn evict(&mut self, pid: u32) {
        self.contenders.retain(|contender| contender.pid() != pid && !contender.finished());
        self.failure = None;
        self.last_spawn = None;
    }

    fn recruit_now(&mut self) {
        self.last_spawn = Some(Instant::now().checked_sub(self.spawn_delay).unwrap_or_else(Instant::now));
    }

    fn reap(&mut self) -> Option<String> {
        let (finished, alive): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.contenders).into_iter().partition(|contender| contender.finished());
        self.contenders = alive;
        if self.failure.is_none() {
            self.failure = finished.iter().find_map(|contender| contender.failure());
        }
        if finished.iter().any(|contender| contender.status().is_some_and(|status| status.code() == Some(0))) {
            self.spawn_delay = (self.spawn_delay * 2).min(MAX_SPAWN_DELAY);
        }
        if self.contenders.is_empty() { self.failure.clone() } else { None }
    }

    fn should_recruit(&mut self, registered: bool) -> bool {
        if self.last_spawn.is_none() && registered {
            self.last_spawn = Some(Instant::now());
        }
        self.failure.is_none()
            && self.contenders.len() < 2
            && self.last_spawn.is_none_or(|last| last.elapsed() >= self.spawn_delay)
    }

    fn add(&mut self, contender: Contender) {
        self.contenders.push(contender);
        self.last_spawn = Some(Instant::now());
    }
}
