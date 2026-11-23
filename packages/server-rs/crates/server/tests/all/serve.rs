#![cfg(unix)]

use std::time::Duration;

use rustix::process::Signal;

use crate::support::{Process, Sandbox, basic_header, get, json_body, port_from_url};

#[test]
fn stdio_serves_one_json_line_and_exits_when_stdin_closes() {
    let sandbox = Sandbox::new();
    let mut process = Process::spawn(&sandbox, &["serve", "--stdio", "--port", "0"], &[("OPENCODE_PASSWORD", "pw")]);
    let handshake = process.line();
    let url = handshake_url(&handshake);
    assert!(url.starts_with("http://127.0.0.1:"), "url does not start with http://127.0.0.1: {url:?}");

    let response = get(port_from_url(&url), "/api/info", &[("Authorization", &basic_header("pw"))]);
    assert_eq!(status_code(&response), "200", "response:\n{response}");
    assert_eq!(json_body(&response)["pid"], serde_json::json!(process.pid()), "response:\n{response}");

    process.close_stdin();
    let status = process.wait(Duration::from_secs(5));
    assert_eq!(status.code(), Some(0), "exit status {status:?}");
}

#[test]
fn stdio_exits_143_on_sigterm_with_stdin_open() {
    let sandbox = Sandbox::new();
    let mut process = Process::spawn(&sandbox, &["serve", "--stdio", "--port", "0"], &[("OPENCODE_PASSWORD", "pw")]);
    let handshake = process.line();
    assert!(handshake_url(&handshake).starts_with("http://127.0.0.1:"), "handshake url: {handshake:?}");

    process.signal(Signal::TERM);
    let status = process.wait(Duration::from_secs(5));
    assert_eq!(status.code(), Some(143), "exit status {status:?}");
}

#[test]
fn stdio_exits_130_on_sigint_with_stdin_open() {
    let sandbox = Sandbox::new();
    let mut process = Process::spawn(&sandbox, &["serve", "--stdio", "--port", "0"], &[("OPENCODE_PASSWORD", "pw")]);
    let handshake = process.line();
    assert!(handshake_url(&handshake).starts_with("http://127.0.0.1:"), "handshake url: {handshake:?}");

    process.signal(Signal::INT);
    let status = process.wait(Duration::from_secs(5));
    assert_eq!(status.code(), Some(130), "exit status {status:?}");
}

#[test]
fn foreground_prints_the_generated_password_and_exits_143_on_sigterm() {
    let sandbox = Sandbox::new();
    let mut process = Process::spawn(&sandbox, &["serve", "--port", "0"], &[]);
    let port = listening_port(&process);
    let password = printed_password(&process);

    let response = get(port, "/api/info", &[("Authorization", &basic_header(&password))]);
    assert_eq!(status_code(&response), "200", "response:\n{response}");
    assert_eq!(json_body(&response)["pid"], serde_json::json!(process.pid()), "response:\n{response}");

    process.signal(Signal::TERM);
    let status = process.wait(Duration::from_secs(5));
    assert_eq!(status.code(), Some(143), "exit status {status:?}");
}

#[test]
fn foreground_exits_130_on_sigint() {
    let sandbox = Sandbox::new();
    let mut process = Process::spawn(&sandbox, &["serve", "--port", "0"], &[]);
    let port = listening_port(&process);
    let password = printed_password(&process);

    let response = get(port, "/api/info", &[("Authorization", &basic_header(&password))]);
    assert_eq!(status_code(&response), "200", "response:\n{response}");

    process.signal(Signal::INT);
    let status = process.wait(Duration::from_secs(5));
    assert_eq!(status.code(), Some(130), "exit status {status:?}");
}

#[test]
fn a_second_signal_during_shutdown_exits_without_hanging() {
    let sandbox = Sandbox::new();
    let mut process = Process::spawn(&sandbox, &["serve", "--port", "0"], &[]);
    let port = listening_port(&process);
    let password = printed_password(&process);

    let response = get(port, "/api/info", &[("Authorization", &basic_header(&password))]);
    assert_eq!(status_code(&response), "200", "response:\n{response}");

    process.signal(Signal::TERM);
    process.signal(Signal::TERM);
    let status = process.wait(Duration::from_secs(5));
    let code = status.code();
    assert!(matches!(code, Some(143) | Some(1)), "exit status {status:?} (code {code:?})");
}

#[test]
fn service_and_stdio_together_exit_one() {
    let sandbox = Sandbox::new();
    let mut process = Process::spawn(&sandbox, &["serve", "--service", "--stdio"], &[]);
    let status = process.wait(Duration::from_secs(5));
    assert_eq!(status.code(), Some(1), "exit status {status:?}");
    let error = process.error_line();
    assert!(error.contains("--service and --stdio cannot be combined"), "stderr line: {error:?}");
}

#[test]
fn without_a_port_it_skips_a_busy_4096() {
    let sandbox = Sandbox::new();
    let Ok(held) = std::net::TcpListener::bind("127.0.0.1:4096") else {
        println!("skipped: 127.0.0.1:4096 is already busy");
        return;
    };
    let Ok(probe) = std::net::TcpListener::bind("127.0.0.1:4097") else {
        println!("skipped: 127.0.0.1:4097 is already busy");
        return;
    };
    drop(probe);

    let mut process = Process::spawn(&sandbox, &["serve"], &[]);
    let line = process.line();
    assert!(line.contains(":4097"), "first line does not name the next free port: {line:?}");

    process.signal(Signal::TERM);
    let status = process.wait(Duration::from_secs(5));
    assert_eq!(status.code(), Some(143), "exit status {status:?}");
    drop(held);
}

#[test]
fn the_cors_flag_allows_its_origin() {
    let sandbox = Sandbox::new();
    let mut process = Process::spawn(&sandbox, &["serve", "--port", "0", "--cors", "https://example.test"], &[]);
    let port = listening_port(&process);
    let password = printed_password(&process);

    let response =
        get(port, "/api/info", &[("Authorization", &basic_header(&password)), ("Origin", "https://example.test")]);
    assert_eq!(status_code(&response), "200", "response:\n{response}");
    let lowered = response.to_ascii_lowercase();
    assert!(
        lowered.contains("access-control-allow-origin: https://example.test"),
        "response has no matching CORS header:\n{response}"
    );

    process.signal(Signal::TERM);
    let status = process.wait(Duration::from_secs(5));
    assert_eq!(status.code(), Some(143), "exit status {status:?}");
}

fn handshake_url(handshake: &str) -> String {
    let value: serde_json::Value = serde_json::from_str(handshake)
        .unwrap_or_else(|error| panic!("first stdout line is not JSON ({error}): {handshake:?}"));
    let object = value.as_object().unwrap_or_else(|| panic!("handshake is not an object: {handshake:?}"));
    assert_eq!(object.len(), 1, "handshake has keys other than url: {handshake:?}");
    let url = object
        .get("url")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("handshake has no string url: {handshake:?}"));
    url.to_owned()
}

fn listening_port(process: &Process) -> u16 {
    let line = process.line();
    let address = line
        .strip_prefix("server listening on ")
        .unwrap_or_else(|| panic!("first line does not start with `server listening on `: {line:?}"));
    port_from_url(address)
}

fn printed_password(process: &Process) -> String {
    let line = process.line();
    let password = line
        .strip_prefix("server password ")
        .unwrap_or_else(|| panic!("second line does not start with `server password `: {line:?}"));
    assert!(!password.is_empty(), "second line has an empty password: {line:?}");
    password.to_owned()
}

fn status_code(response: &str) -> &str {
    response.lines().next().unwrap_or_default().split_whitespace().nth(1).unwrap_or_default()
}
