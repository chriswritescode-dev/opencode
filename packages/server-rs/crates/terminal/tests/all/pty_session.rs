use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use opencode_contract::codec::JsonMap;
use opencode_contract::generated::PtyStatus;
use opencode_contract::generated::requests::PtyCreateBody;
use opencode_terminal::pty::{
    AttachInput, Attachment, BUFFER_LIMIT, CreateInput, Pty, PtyError, UpdateInput, UpdateSize,
};
use serde_json::Value;

use crate::{Harness, which};

const TIMEOUT: Duration = Duration::from_secs(10);

fn input(command: &str, args: &[&str], cwd: &str) -> CreateInput {
    PtyCreateBody {
        command: Some(command.to_owned()),
        args: Some(args.iter().map(|arg| (*arg).to_owned()).collect()),
        cwd: Some(cwd.to_owned()),
        title: None,
        env: Some(JsonMap(vec![
            ("TERM".to_owned(), "xterm-256color".to_owned()),
            ("OPENCODE_TERMINAL".to_owned(), "1".to_owned()),
        ])),
    }
}

fn attach_deferred(pty: &Pty, id: &str, cursor: Option<i64>) -> (Attachment, Receiver<String>, Receiver<Option<u32>>) {
    let (data, output) = channel();
    let (ended, end) = channel();
    let attachment = pty
        .attach(
            id,
            AttachInput {
                cursor,
                on_data: Arc::new(move |chunk| {
                    data.send(chunk).ok();
                }),
                on_end: Arc::new(move |code| {
                    ended.send(code).ok();
                }),
            },
        )
        .expect("attach");
    (attachment, output, end)
}

fn attach(pty: &Pty, id: &str, cursor: Option<i64>) -> (Attachment, Receiver<String>, Receiver<Option<u32>>) {
    let (attachment, output, end) = attach_deferred(pty, id, cursor);
    attachment.activate();
    (attachment, output, end)
}

fn wait_output(output: &Receiver<String>, text: &mut String, needle: &str) {
    let deadline = Instant::now() + TIMEOUT;
    while !text.contains(needle) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let chunk = match output.recv_timeout(remaining) {
            Ok(chunk) => chunk,
            Err(error) => panic!("output timeout ({error}) waiting for {needle:?} in {text:?}"),
        };
        text.push_str(&chunk);
    }
}

fn wait_events(events: &Receiver<(String, Value)>, id: &str, count: usize) -> Vec<String> {
    let deadline = Instant::now() + TIMEOUT;
    let mut picked = Vec::new();
    while picked.len() < count {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let (kind, data) = events.recv_timeout(remaining).expect("an event");
        if event_id(&data) == Some(id) {
            picked.push(kind);
        }
    }
    picked
}

fn event_id(data: &Value) -> Option<&str> {
    data.get("info").and_then(|info| info.get("id")).or_else(|| data.get("id")).and_then(Value::as_str)
}

fn wait_until(condition: impl Fn() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !condition() {
        assert!(Instant::now() < deadline, "condition was not met in time");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn returns_typed_not_found_errors_for_missing_sessions() {
    let harness = Harness::new("/tmp");
    let id = "pty_missing";
    let errors = [
        harness.pty.get(id).map(|_| ()),
        harness.pty.update(id, UpdateInput { title: Some("missing".to_owned()), size: None }).map(|_| ()),
        harness.pty.remove(id),
        harness.pty.write(id, "input"),
        harness
            .pty
            .attach(id, AttachInput { cursor: None, on_data: Arc::new(|_| {}), on_end: Arc::new(|_| {}) })
            .map(|_| ()),
    ];
    for error in errors {
        match error {
            Err(PtyError::NotFound { pty_id }) => assert_eq!(pty_id, id),
            other => panic!("expected not found, got {other:?}"),
        }
    }
}

#[test]
fn retains_exited_sessions_until_removed() {
    let harness = Harness::new("/tmp");
    let (events, _handle) = harness.events();
    let info = harness.pty.create(input("/usr/bin/env", &["sh", "-c", "exit 3"], "/tmp")).expect("create");

    assert_eq!(wait_events(&events, &info.id.0, 2), vec!["pty.created".to_owned(), "pty.exited".to_owned()]);
    let exited = harness.pty.get(&info.id.0).expect("get");
    assert_eq!(exited.status, PtyStatus::Exited);
    assert_eq!(exited.exit_code, Some(3.0));

    harness.pty.remove(&info.id.0).expect("remove");
    assert_eq!(wait_events(&events, &info.id.0, 1), vec!["pty.deleted".to_owned()]);
    assert!(matches!(harness.pty.get(&info.id.0), Err(PtyError::NotFound { .. })));
}

#[test]
fn replays_buffered_output_and_streams_live_output() {
    let harness = Harness::new("/tmp");
    let info = harness.pty.create(input("cat", &[], "/tmp")).expect("create");
    harness.pty.write(&info.id.0, "AAA\n").expect("write");

    let (first, output, _end) = attach(&harness.pty, &info.id.0, None);
    let mut text = String::new();
    wait_output(&output, &mut text, "AAA");
    assert!(text.contains("AAA"));

    first.write("BBB\n".to_owned());
    wait_output(&output, &mut text, "BBB");

    let (replayed, _replayed_output, _end) = attach(&harness.pty, &info.id.0, None);
    assert!(replayed.replay.contains("AAA"));
    assert!(replayed.replay.contains("BBB"));
    assert!(replayed.cursor > 0);

    let (tail, _tail_output, _end) = attach(&harness.pty, &info.id.0, Some(-1));
    assert!(tail.replay.is_empty());
    assert_eq!(tail.cursor, replayed.cursor);

    harness.pty.remove(&info.id.0).expect("remove");
}

#[test]
fn reconnect_with_a_cursor_replays_only_new_output() {
    let harness = Harness::new("/tmp");
    let info = harness
        .pty
        .create(input("/usr/bin/env", &["sh", "-c", "stty -echo; echo READY; exec cat"], "/tmp"))
        .expect("create");
    let (ready, ready_output, _end) = attach(&harness.pty, &info.id.0, None);
    let mut text = String::new();
    wait_output(&ready_output, &mut text, "READY");
    ready.detach();

    harness.pty.write(&info.id.0, "AAA\n").expect("write");

    let (first, output, _end) = attach(&harness.pty, &info.id.0, None);
    let mut text = String::new();
    wait_output(&output, &mut text, "AAA");
    first.detach();

    let (tail, tail_output, _end) = attach(&harness.pty, &info.id.0, Some(-1));
    let cursor = tail.cursor;
    harness.pty.write(&info.id.0, "BBB\n").expect("write");
    let mut text = String::new();
    wait_output(&tail_output, &mut text, "BBB");
    tail.detach();

    let (reconnected, _output, _end) = attach(&harness.pty, &info.id.0, Some(cursor as i64));
    assert!(!reconnected.replay.contains("AAA"), "replay: {:?}", reconnected.replay);
    assert!(reconnected.replay.contains("BBB"), "replay: {:?}", reconnected.replay);

    harness.pty.remove(&info.id.0).expect("remove");
}

#[test]
fn stops_delivering_output_after_detach() {
    let harness = Harness::new("/tmp");
    let info = harness.pty.create(input("cat", &[], "/tmp")).expect("create");
    let (attached, detached_output, _end) = attach(&harness.pty, &info.id.0, Some(-1));

    attached.detach();
    harness.pty.write(&info.id.0, "AAA\n").expect("write");

    let (_verify, verify_output, _end) = attach(&harness.pty, &info.id.0, None);
    let mut text = String::new();
    wait_output(&verify_output, &mut text, "AAA");
    assert!(detached_output.try_recv().is_err());

    harness.pty.remove(&info.id.0).expect("remove");
}

#[test]
fn isolates_output_between_sessions() {
    let harness = Harness::new("/tmp");
    let a = harness.pty.create(input("cat", &[], "/tmp")).expect("create a");
    let b = harness.pty.create(input("cat", &[], "/tmp")).expect("create b");
    let (_attached_a, output_a, _end) = attach(&harness.pty, &a.id.0, None);
    let (_attached_b, output_b, _end) = attach(&harness.pty, &b.id.0, None);

    harness.pty.write(&a.id.0, "AAA\n").expect("write");
    let mut text = String::new();
    wait_output(&output_a, &mut text, "AAA");
    assert!(output_b.try_recv().is_err());

    harness.pty.remove(&a.id.0).expect("remove a");
    harness.pty.remove(&b.id.0).expect("remove b");
}

#[test]
fn notifies_attachments_with_the_exit_code_and_rejects_attach_after_exit() {
    let harness = Harness::new("/tmp");
    let (events, _handle) = harness.events();
    let info = harness.pty.create(input("cat", &[], "/tmp")).expect("create");
    let (_attached, _output, ended) = attach(&harness.pty, &info.id.0, None);

    harness.pty.write(&info.id.0, "\u{4}").expect("write");
    assert_eq!(ended.recv_timeout(TIMEOUT).expect("exit"), Some(0));
    wait_events(&events, &info.id.0, 2);

    match harness
        .pty
        .attach(&info.id.0, AttachInput { cursor: None, on_data: Arc::new(|_| {}), on_end: Arc::new(|_| {}) })
    {
        Err(PtyError::Exited { pty_id }) => assert_eq!(pty_id, info.id.0),
        other => panic!("expected exited, got {:?}", other.map(|_| ())),
    }
    harness.pty.remove(&info.id.0).expect("remove");
}

#[test]
fn activates_after_exit_and_delivers_pending_output_and_exit_code() {
    let harness = Harness::new("/tmp");
    let info =
        harness.pty.create(input("/usr/bin/env", &["sh", "-c", "read line; echo hi; exit 7"], "/tmp")).expect("create");
    let (attachment, output, ended) = attach_deferred(&harness.pty, &info.id.0, None);
    harness.pty.write(&info.id.0, "\n").expect("write");
    wait_until(|| matches!(harness.pty.get(&info.id.0), Ok(ref info) if info.status == PtyStatus::Exited));

    attachment.activate();
    let mut text = String::new();
    wait_output(&output, &mut text, "hi");
    assert_eq!(ended.recv_timeout(TIMEOUT).expect("exit"), Some(7));
    assert!(ended.try_recv().is_err(), "on_end must fire exactly once");
}

#[test]
fn activating_after_remove_reports_a_detached_end_once() {
    let harness = Harness::new("/tmp");
    let info = harness.pty.create(input("cat", &[], "/tmp")).expect("create");
    let (attachment, _output, ended) = attach_deferred(&harness.pty, &info.id.0, None);
    harness.pty.remove(&info.id.0).expect("remove");

    attachment.activate();
    assert_eq!(ended.recv_timeout(TIMEOUT).expect("end"), None);
    assert!(ended.try_recv().is_err(), "on_end must fire exactly once");
}

#[test]
fn defaults_command_login_args_and_cwd_from_shell_selection() {
    let bash = which("bash").expect("bash on PATH");
    let harness = Harness::new("/tmp");
    harness.shell.configure(Some(&bash));
    let info = harness
        .pty
        .create(CreateInput { command: None, args: None, cwd: None, title: Some("configured".to_owned()), env: None })
        .expect("create");
    assert_eq!(info.command, bash);
    assert_eq!(info.args, ["-l"]);
    assert_eq!(info.cwd, "/tmp");
    assert_eq!(info.title, "configured");
    harness.pty.remove(&info.id.0).expect("remove");
}

#[test]
fn retains_at_most_the_utf16_buffer_cap_for_non_ascii_output() {
    let harness = Harness::new("/tmp");
    let yes = which("yes").expect("yes on PATH");
    let info = harness.pty.create(input(&yes, &["é"], "/tmp")).expect("create");
    let (live, output, _end) = attach(&harness.pty, &info.id.0, Some(-1));

    let mut units = 0;
    while units <= BUFFER_LIMIT {
        let chunk = output.recv_timeout(TIMEOUT).expect("output");
        units += chunk.encode_utf16().count();
    }
    live.detach();

    let (replayed, _replayed_output, _end) = attach(&harness.pty, &info.id.0, None);
    let units = replayed.replay.encode_utf16().count();
    assert!(units <= BUFFER_LIMIT, "retained {units} units");
    assert!(units >= BUFFER_LIMIT - 2, "retained {units} units");
    assert!(replayed.replay.chars().all(|character| matches!(character, 'é' | '\n' | '\r')));
    harness.pty.remove(&info.id.0).expect("remove");
}

#[test]
fn evicts_exited_sessions_after_the_retention_limit() {
    let harness = Harness::new("/tmp");
    let mut created = Vec::new();
    for _ in 0..26 {
        let info = harness.pty.create(input("/usr/bin/env", &["sh", "-c", "exit 0"], "/tmp")).expect("create");
        created.push(info);
    }
    for info in &created {
        wait_until(|| matches!(harness.pty.get(&info.id.0), Ok(ref info) if info.status == PtyStatus::Exited));
    }
    wait_until(|| harness.pty.list().len() == 25);
    assert_eq!(harness.pty.list().len(), 25);
    assert!(created.iter().any(|info| matches!(harness.pty.get(&info.id.0), Err(PtyError::NotFound { .. }))));
}

#[test]
fn updates_titles_and_sizes_and_reports_missing_sessions() {
    let harness = Harness::new("/tmp");
    let info = harness.pty.create(input("cat", &[], "/tmp")).expect("create");
    let updated = harness
        .pty
        .update(
            &info.id.0,
            UpdateInput { title: Some("renamed".to_owned()), size: Some(UpdateSize { rows: 40.0, cols: 120.0 }) },
        )
        .expect("update");
    assert_eq!(updated.title, "renamed");
    harness.pty.remove(&info.id.0).expect("remove");
    assert!(matches!(
        harness.pty.update(&info.id.0, UpdateInput { title: None, size: None }),
        Err(PtyError::NotFound { .. })
    ));
}
