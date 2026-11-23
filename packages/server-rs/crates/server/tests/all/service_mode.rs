#![cfg(unix)]

use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustix::process::Signal;

use crate::support::{Process, Sandbox, basic_header, get, json_body};

#[test]
fn service_registers_announces_and_cleans_up_on_sigterm() {
    let sandbox = Sandbox::new();
    let port = free_port();
    write_config(&sandbox, port);
    let mut process = start(&sandbox);

    let url = listening_url(&process);
    assert_eq!(url, format!("http://127.0.0.1:{port}"), "first line url: {url:?}");

    let registration_path = registration_file(&sandbox);
    wait_for(&registration_path, Duration::from_secs(10));
    let registration = read_json(&registration_path);
    let object = registration.as_object().unwrap_or_else(|| panic!("registration is not an object: {registration}"));
    let keys: Vec<&str> = object.keys().map(String::as_str).collect();
    assert_eq!(keys, ["id", "version", "url", "pid", "password"], "registration: {registration}");

    let mode = std::fs::metadata(&registration_path)
        .unwrap_or_else(|error| panic!("metadata {}: {error}", registration_path.display()))
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "registration mode {mode:o}: {registration}");

    assert!(!string_field(&registration, "id").is_empty(), "registration id is empty: {registration}");
    assert_eq!(string_field(&registration, "url"), url, "registration: {registration}");
    assert_eq!(registration["pid"], serde_json::json!(process.pid()), "registration: {registration}");
    let version = expected_version();
    assert_eq!(string_field(&registration, "version"), version.as_str(), "registration: {registration}");
    let password = string_field(&registration, "password");
    assert!(!password.is_empty(), "registration password is empty: {registration}");

    let config = read_json(&config_file(&sandbox));
    assert_eq!(string_field(&config, "password"), password, "service config: {config}");

    let response = get(port, "/api/info", &[("Authorization", &basic_header(password))]);
    assert_eq!(status_code(&response), "200", "response:\n{response}");
    assert_eq!(json_body(&response)["pid"], serde_json::json!(process.pid()), "response:\n{response}");

    process.signal(Signal::TERM);
    let status = process.wait(Duration::from_secs(5));
    assert_eq!(status.code(), Some(143), "exit status {status:?}");
    assert!(!registration_path.exists(), "registration still exists: {}", registration_path.display());
}

#[test]
fn two_contenders_elect_one_service_and_the_loser_exits_zero() {
    let sandbox = Sandbox::new();
    let port = free_port();
    write_config(&sandbox, port);
    let first = start(&sandbox);
    let second = start(&sandbox);

    let registration_path = registration_file(&sandbox);
    wait_for(&registration_path, Duration::from_secs(10));
    let registration = read_json(&registration_path);
    let registered =
        registration["pid"].as_u64().unwrap_or_else(|| panic!("registration pid is not a number: {registration}"));
    let (first_pid, second_pid) = (u64::from(first.pid()), u64::from(second.pid()));
    assert!(
        registered == first_pid || registered == second_pid,
        "registered pid {registered} is neither {first_pid} nor {second_pid}"
    );

    let (mut winner, mut loser) = if registered == first_pid { (first, second) } else { (second, first) };
    let status = loser.wait(Duration::from_secs(10));
    assert_eq!(status.code(), Some(0), "loser exit status {status:?}");

    winner.signal(Signal::TERM);
    let status = winner.wait(Duration::from_secs(5));
    assert_eq!(status.code(), Some(143), "winner exit status {status:?}");
    assert!(!registration_path.exists(), "registration still exists: {}", registration_path.display());
}

#[test]
fn ipv6_contenders_elect_one_service_and_the_loser_exits_zero() {
    let Ok(held) = TcpListener::bind(("[::1]", 0)) else {
        return;
    };
    let port = held.local_addr().expect("ipv6 free port address").port();
    drop(held);

    let sandbox = Sandbox::new();
    write_config_hostname(&sandbox, "::1", port);
    let first = start(&sandbox);
    let second = start(&sandbox);

    let registration_path = registration_file(&sandbox);
    wait_for(&registration_path, Duration::from_secs(10));
    let registration = read_json(&registration_path);
    assert_eq!(string_field(&registration, "url"), format!("http://[::1]:{port}"), "registration: {registration}");
    let registered =
        registration["pid"].as_u64().unwrap_or_else(|| panic!("registration pid is not a number: {registration}"));
    let (first_pid, second_pid) = (u64::from(first.pid()), u64::from(second.pid()));
    assert!(
        registered == first_pid || registered == second_pid,
        "registered pid {registered} is neither {first_pid} nor {second_pid}"
    );

    let (mut winner, mut loser) = if registered == first_pid { (first, second) } else { (second, first) };
    let status = loser.wait(Duration::from_secs(10));
    assert_eq!(status.code(), Some(0), "loser exit status {status:?}");

    winner.signal(Signal::TERM);
    let status = winner.wait(Duration::from_secs(5));
    assert_eq!(status.code(), Some(143), "winner exit status {status:?}");
    assert!(!registration_path.exists(), "registration still exists: {}", registration_path.display());
}

#[test]
fn sigterm_during_the_election_exits_with_143() {
    let sandbox = Sandbox::new();
    let held = TcpListener::bind("127.0.0.1:0").expect("bind a held port");
    let port = held.local_addr().expect("held port address").port();
    write_config(&sandbox, port);
    let mut process = start(&sandbox);

    std::thread::sleep(Duration::from_secs(2));
    process.signal(Signal::TERM);
    let status = process.wait(Duration::from_secs(5));
    assert_eq!(status.code(), Some(143), "exit status {status:?}");
    drop(held);
}

#[test]
fn a_replaced_registration_makes_the_service_exit_zero() {
    let sandbox = Sandbox::new();
    let port = free_port();
    write_config(&sandbox, port);
    let mut process = start(&sandbox);
    listening_url(&process);

    let registration_path = registration_file(&sandbox);
    wait_for(&registration_path, Duration::from_secs(10));
    let mut registration = read_json(&registration_path);
    registration["id"] = serde_json::json!("someone-else");
    std::fs::write(&registration_path, serde_json::to_string(&registration).unwrap())
        .unwrap_or_else(|error| panic!("write {}: {error}", registration_path.display()));

    let status = process.wait(Duration::from_secs(7));
    assert_eq!(status.code(), Some(0), "exit status {status:?}");
    assert_eq!(process.error_line(), "managed service registration replaced; shutting down");

    assert!(registration_path.exists(), "registration was removed: {}", registration_path.display());
    let left = read_json(&registration_path);
    assert_eq!(string_field(&left, "id"), "someone-else", "registration: {left}");
}

#[test]
fn the_password_is_persisted_and_reused_across_restarts() {
    let sandbox = Sandbox::new();
    let port = free_port();
    write_config(&sandbox, port);

    let mut process = start(&sandbox);
    listening_url(&process);
    let registration_path = registration_file(&sandbox);
    wait_for(&registration_path, Duration::from_secs(10));
    let first = string_field(&read_json(&registration_path), "password").to_owned();
    assert!(!first.is_empty(), "registration password is empty: {first:?}");
    let config = read_json(&config_file(&sandbox));
    assert_eq!(string_field(&config, "password"), first, "after the first start");

    process.signal(Signal::TERM);
    let status = process.wait(Duration::from_secs(5));
    assert_eq!(status.code(), Some(143), "exit status {status:?}");
    assert!(!registration_path.exists(), "registration still exists: {}", registration_path.display());

    let mut process = start(&sandbox);
    listening_url(&process);
    wait_for(&registration_path, Duration::from_secs(10));
    let registration = read_json(&registration_path);
    let second = string_field(&registration, "password");
    assert_eq!(second, first, "second registration: {second:?}");

    process.signal(Signal::TERM);
    let status = process.wait(Duration::from_secs(5));
    assert_eq!(status.code(), Some(143), "exit status {status:?}");
}

#[test]
#[ignore]
fn a_held_port_fails_after_the_election_window() {
    let sandbox = Sandbox::new();
    let held = TcpListener::bind("127.0.0.1:0").expect("bind a held port");
    let port = held.local_addr().expect("held port address").port();
    write_config(&sandbox, port);
    let mut process = start(&sandbox);

    let status = process.wait(Duration::from_secs(30));
    assert_eq!(status.code(), Some(1), "exit status {status:?}");
    let expected = format!(
        "Managed service port {port} on 127.0.0.1 is already in use by another process. Configure another port with `opencode service set port <port>` and start the service again."
    );
    assert_eq!(process.error_line(), expected);
    drop(held);
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a free port");
    listener.local_addr().expect("free port address").port()
}

fn config_file(sandbox: &Sandbox) -> PathBuf {
    sandbox.dir().join("config/opencode/service-local.json")
}

fn registration_file(sandbox: &Sandbox) -> PathBuf {
    sandbox.dir().join("state/opencode/service-local.json")
}

fn write_config(sandbox: &Sandbox, port: u16) {
    write_config_text(sandbox, &format!("{{\"port\":{port}}}"));
}

fn write_config_hostname(sandbox: &Sandbox, hostname: &str, port: u16) {
    write_config_text(sandbox, &format!("{{\"hostname\":\"{hostname}\",\"port\":{port}}}"));
}

fn write_config_text(sandbox: &Sandbox, contents: &str) {
    let path = config_file(sandbox);
    std::fs::create_dir_all(path.parent().expect("config file parent")).unwrap();
    std::fs::write(&path, contents).unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
}

fn wait_for(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while !path.exists() {
        if Instant::now() >= deadline {
            panic!("{} did not appear within {timeout:?}", path.display());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn read_json(path: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{} is not JSON ({error}): {text}", path.display()))
}

fn start(sandbox: &Sandbox) -> Process {
    Process::spawn(sandbox, &["serve", "--service"], &[])
}

fn expected_version() -> String {
    option_env!("OPENCODE_VERSION").unwrap_or("local").to_owned()
}

fn listening_url(process: &Process) -> String {
    let line = process.line();
    let url = line
        .strip_prefix("server listening on ")
        .unwrap_or_else(|| panic!("first line does not start with `server listening on `: {line:?}"));
    url.to_owned()
}

fn string_field<'a>(value: &'a serde_json::Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or_else(|| panic!("{key} is not a string in {value}"))
}

fn status_code(response: &str) -> &str {
    response.lines().next().unwrap_or_default().split_whitespace().nth(1).unwrap_or_default()
}
