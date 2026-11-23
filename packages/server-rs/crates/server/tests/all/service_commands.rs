#![cfg(unix)]

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process};

use crate::support::Sandbox;

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const REGISTRATION_TIMEOUT: Duration = Duration::from_secs(10);

#[test]
fn service_start_status_restart_and_stop_follow_the_registration() {
    let sandbox = Sandbox::new();
    let port = free_port();
    write_config(&sandbox, port);
    let _guard = ServiceGuard::new(registration_file(&sandbox));
    let url = format!("http://127.0.0.1:{port}");

    let output = run(&sandbox, &["service", "status"]);
    assert_eq!(output.status.code(), Some(0), "status exit with no service: {output:?}");
    assert_eq!(stdout(&output), "stopped", "status with no service");

    let output = run(&sandbox, &["service", "start"]);
    assert_eq!(output.status.code(), Some(0), "start exit: {output:?}");
    assert_eq!(stdout(&output), url, "start url");

    let registration_path = registration_file(&sandbox);
    wait_for_registration(&registration_path);
    let registration = read_json(&registration_path);
    assert_eq!(registration["url"], serde_json::json!(url), "registration url: {registration}");
    let pid = pid_of(&registration);
    assert!(pid > 0, "registration pid is not positive: {registration}");

    let output = run(&sandbox, &["service", "start"]);
    assert_eq!(output.status.code(), Some(0), "second start exit: {output:?}");
    assert_eq!(stdout(&output), url, "second start url");
    assert_eq!(pid_of(&read_json(&registration_path)), pid, "second start spawned another service");

    let output = run(&sandbox, &["service", "status"]);
    assert_eq!(output.status.code(), Some(0), "status exit: {output:?}");
    assert_eq!(stdout(&output), url, "status url");

    let output = run(&sandbox, &["service", "restart"]);
    assert_eq!(output.status.code(), Some(0), "restart exit: {output:?}");
    assert_eq!(stdout(&output), url, "restart url");

    wait_for_registration(&registration_path);
    let restarted = pid_of(&read_json(&registration_path));
    assert_ne!(restarted, pid, "restart kept the previous pid {pid}");

    let output = run(&sandbox, &["service", "stop"]);
    assert_eq!(output.status.code(), Some(0), "stop exit: {output:?}");
    assert_eq!(stdout(&output), "", "stop printed output");
    assert!(!registration_path.exists(), "registration still exists: {}", registration_path.display());

    let output = run(&sandbox, &["service", "status"]);
    assert_eq!(output.status.code(), Some(0), "status after stop exit: {output:?}");
    assert_eq!(stdout(&output), "stopped", "status after stop");
}

#[test]
fn service_stop_without_a_registration_succeeds() {
    let sandbox = Sandbox::new();
    let output = run(&sandbox, &["service", "stop"]);
    assert_eq!(output.status.code(), Some(0), "stop without a service: {output:?}");
    assert_eq!(stdout(&output), "", "stop without a service printed output");
}

#[test]
fn service_config_get_set_and_unset_match_the_cli() {
    let sandbox = Sandbox::new();
    let _guard = ServiceGuard::new(registration_file(&sandbox));

    let output = run(&sandbox, &["service", "get"]);
    assert_eq!(output.status.code(), Some(0), "get exit: {output:?}");
    assert_eq!(stdout(&output), "{}", "get with no config");

    let port = free_port();
    let port_text = port.to_string();
    let output = run(&sandbox, &["service", "set", "port", &port_text]);
    assert_eq!(output.status.code(), Some(0), "set port exit: {output:?}");
    assert_eq!(stdout(&output), "", "set port printed output");

    let output = run(&sandbox, &["service", "get", "port"]);
    assert_eq!(output.status.code(), Some(0), "get port exit: {output:?}");
    assert_eq!(stdout(&output), port_text, "get port");

    let output = run(&sandbox, &["service", "set", "port", "0"]);
    assert_eq!(output.status.code(), Some(1), "set invalid port exit: {output:?}");
    assert_eq!(stdout(&output), "", "set invalid port printed output");
    assert_eq!(stderr(&output), "Port must be between 1 and 65535", "set invalid port error");

    let output = run(&sandbox, &["service", "get", "password"]);
    assert_eq!(output.status.code(), Some(0), "get password exit: {output:?}");
    let password = stdout(&output);
    assert!(!password.is_empty(), "get password printed nothing");
    let config = read_json(&config_file(&sandbox));
    assert_eq!(config["password"], serde_json::json!(password), "service config: {config}");

    let output = run(&sandbox, &["service", "set", "env", "FOO", "bar"]);
    assert_eq!(output.status.code(), Some(0), "set env exit: {output:?}");
    let output = run(&sandbox, &["service", "get", "env", "FOO"]);
    assert_eq!(output.status.code(), Some(0), "get env exit: {output:?}");
    assert_eq!(stdout(&output), "bar", "get env FOO");
    let output = run(&sandbox, &["service", "unset", "env", "FOO"]);
    assert_eq!(output.status.code(), Some(0), "unset env exit: {output:?}");
    let output = run(&sandbox, &["service", "get", "env", "FOO"]);
    assert_eq!(output.status.code(), Some(0), "get unset env exit: {output:?}");
    assert_eq!(stdout(&output), "", "get unset env FOO");

    let output = run(&sandbox, &["service", "get", "nope"]);
    assert_eq!(output.status.code(), Some(1), "get unknown key exit: {output:?}");
    assert_eq!(stdout(&output), "", "get unknown key printed output");
    assert_eq!(stderr(&output), "Unknown service config key: nope", "get unknown key error");
}

fn run(sandbox: &Sandbox, args: &[&str]) -> Output {
    let root = sandbox.dir();
    Command::new(env!("CARGO_BIN_EXE_opencode-server"))
        .args(args)
        .current_dir(root)
        .env_clear()
        .env("HOME", root.join("home"))
        .env("TMPDIR", root.join("tmp"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("XDG_STATE_HOME", root.join("state"))
        .output()
        .unwrap_or_else(|error| panic!("run opencode-server {args:?}: {error}"))
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim_end_matches(['\n', '\r']).to_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim_end_matches(['\n', '\r']).to_owned()
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a free port");
    let port = listener.local_addr().expect("free port address").port();
    drop(listener);
    port
}

fn config_file(sandbox: &Sandbox) -> PathBuf {
    sandbox.dir().join("config/opencode/service-local.json")
}

fn registration_file(sandbox: &Sandbox) -> PathBuf {
    sandbox.dir().join("state/opencode/service-local.json")
}

fn write_config(sandbox: &Sandbox, port: u16) {
    let path = config_file(sandbox);
    std::fs::create_dir_all(path.parent().expect("config file parent"))
        .unwrap_or_else(|error| panic!("create {}: {error}", path.parent().unwrap().display()));
    std::fs::write(&path, format!("{{\"port\":{port}}}"))
        .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
}

fn read_json(path: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{} is not JSON ({error}): {text}", path.display()))
}

fn pid_of(registration: &serde_json::Value) -> u32 {
    registration["pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .unwrap_or_else(|| panic!("registration pid is not a number: {registration}"))
}

fn wait_for_registration(path: &Path) {
    let deadline = Instant::now() + REGISTRATION_TIMEOUT;
    while !path.exists() {
        if Instant::now() >= deadline {
            panic!("{} did not appear within {REGISTRATION_TIMEOUT:?}", path.display());
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

struct ServiceGuard {
    registration: PathBuf,
}

impl ServiceGuard {
    fn new(registration: PathBuf) -> Self {
        Self { registration }
    }
}

impl Drop for ServiceGuard {
    fn drop(&mut self) {
        let Ok(text) = std::fs::read_to_string(&self.registration) else { return };
        let Ok(registration) = serde_json::from_str::<serde_json::Value>(&text) else { return };
        let Some(pid) = registration["pid"].as_u64().and_then(|pid| i32::try_from(pid).ok()).and_then(Pid::from_raw)
        else {
            return;
        };
        let _ = kill_process(pid, Signal::KILL);
    }
}
