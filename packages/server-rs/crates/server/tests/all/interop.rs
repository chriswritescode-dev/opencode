#![cfg(unix)]

use std::ffi::OsString;
use std::net::TcpListener;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use opencode_api::transport::{Reqwest, Transport};
use opencode_local::paths::Paths;
use opencode_local::registration;
use opencode_service::discover;
use opencode_service::ensure::{self, Options};
use opencode_service::probe::{self, State};
use opencode_service::stop::{self, Poll, Pty};
use opencode_service::ts_command::TsCommand;
use rustix::process::{Pid, Signal, kill_process, test_kill_process};

use crate::support::Sandbox;

#[tokio::test(flavor = "multi_thread")]
async fn the_service_crate_discovers_probes_and_stops_the_server_binary() {
    let ts = TsCommand::from_vars(|name| {
        (name == TsCommand::VAR).then_some(OsString::from(env!("CARGO_BIN_EXE_opencode-server")))
    });
    let sandbox = Sandbox::new();
    let root = sandbox.dir();
    let port = free_port();
    let config = root.join("config/opencode/service-local.json");
    std::fs::create_dir_all(config.parent().expect("config parent")).expect("create the config directory");
    std::fs::write(&config, format!("{{\"port\":{port}}}\n")).expect("write the service config");
    let env: Vec<(String, String)> = [
        ("HOME", root.join("home")),
        ("TMPDIR", root.join("tmp")),
        ("XDG_CONFIG_HOME", root.join("config")),
        ("XDG_DATA_HOME", root.join("data")),
        ("XDG_CACHE_HOME", root.join("cache")),
        ("XDG_STATE_HOME", root.join("state")),
    ]
    .map(|(name, path)| (name.to_owned(), path.to_str().expect("a UTF-8 sandbox path").to_owned()))
    .to_vec();
    let file = root.join("state/opencode/service-local.json");
    let state = root.join("state/opencode");
    std::fs::create_dir_all(&state).expect("create the state directory");
    let transport: Arc<dyn Transport> = Arc::new(Reqwest::default());
    let mut options = Options::new(file.clone(), state, &ts);
    options.env = env.clone();
    let ensured = ensure::ensure(transport.clone(), &options, &mut |_| {}).await.expect("ensure the service starts");
    let url = format!("http://127.0.0.1:{port}");
    let mut cleanup = Cleanup::new(ensured.service.registration.pid);
    assert_eq!(ensured.file, file);
    assert_eq!(ensured.service.endpoint.url, url);

    let paths =
        Paths::from_vars(|name| env.iter().find(|(key, _)| key == name).map(|(_, value)| OsString::from(value)))
            .expect("paths from the isolated environment");
    let found = discover::discover(&*transport, &paths, None).await.expect("discover the started service");
    assert_eq!(found.service.registration.pid, ensured.service.registration.pid);
    assert_eq!(found.file, file);

    let registration = registration::read(&file).expect("read the registration");
    let probed = probe::probe(&*transport, &registration, Duration::from_secs(2)).await;
    let service = probed.service.expect("the service answered the probe");
    assert_eq!(service.state, State::Ready);
    assert!(service.compatible);
    assert!(!probed.timed_out);

    let printed = service_status(&env);
    assert_eq!(printed, url);

    stop::stop(transport.clone(), &file, Pty::Clear, Poll::default()).await.expect("stop the service");
    assert!(!file.exists());
    let printed = service_status(&env);
    assert_eq!(printed, "stopped");
    cleanup.disarm();
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a free port");
    let port = listener.local_addr().expect("the free port address").port();
    drop(listener);
    port
}

fn service_status(env: &[(String, String)]) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_opencode-server"));
    command.args(["service", "status"]).env_clear();
    for (name, value) in env {
        command.env(name, value);
    }
    let output = command.output().expect("run opencode-server service status");
    String::from_utf8_lossy(&output.stdout).trim_end_matches(['\n', '\r']).to_owned()
}

struct Cleanup {
    pid: Pid,
    armed: bool,
}

impl Cleanup {
    fn new(pid: u32) -> Self {
        let pid = i32::try_from(pid).ok().and_then(Pid::from_raw).expect("the registered pid is a valid process id");
        Self { pid, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let _ = kill_process(self.pid, Signal::TERM);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if test_kill_process(self.pid).is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = kill_process(self.pid, Signal::KILL);
    }
}
