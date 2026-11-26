use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use opencode_bus::{Bus, BusError, BusOptions, Event};
use opencode_config::shell::{Search, ShellSelect};
use opencode_contract::generated::{AbsolutePath, LocationRef, ShellInfo, ShellInfoStatus, ShellOutputResponseData};
use opencode_shell::output::Limits;
use opencode_shell::service::{CreateInput, Options, Shell, ShellError, StopReason, ToolCall};
use opencode_storage::{Database, DatabaseLocation};
use opencode_util::lock;
use opencode_util::temp::TempDir;
use serde_json::Value;

pub struct Harness {
    _dir: TempDir,
    _listener: opencode_bus::ListenerHandle,
    pub shell: Arc<Shell>,
    pub data: PathBuf,
    pub project: String,
    events: Mutex<Receiver<(String, Value)>>,
}

impl Harness {
    pub fn new() -> Self {
        let dir = TempDir::new().expect("a temporary directory");
        let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
        let bus = Bus::new(Arc::new(database), BusOptions::default());
        let (sender, receiver) = channel();
        let listener = bus.listen(Box::new(move |event: &Event| -> Result<(), BusError> {
            sender.send((event.r#type().to_owned(), event.data().clone())).ok();
            Ok(())
        }));
        let shell_select = Arc::new(ShellSelect::new(
            Search { bin: PathBuf::from("/usr/bin"), path: std::env::var_os("PATH"), gitbash: None },
            |name| std::env::var_os(name),
        ));
        let project = "prj_shell".to_owned();
        let data = dir.path().join("data");
        let shell = Shell::new(
            bus,
            Options {
                location: LocationRef {
                    directory: AbsolutePath(dir.path().to_string_lossy().into_owned()),
                    workspace_id: None,
                },
                project_id: project.clone(),
                data: data.clone(),
                env: environment(),
                shell: shell_select,
                limits: Arc::new(Limits::default),
            },
        );
        Self { _dir: dir, _listener: listener, shell, data, project, events: Mutex::new(receiver) }
    }

    pub fn input(&self, command: &str) -> CreateInput {
        CreateInput {
            command: command.to_owned(),
            cwd: None,
            timeout: None,
            metadata: None,
            shell: Some("/bin/sh".to_owned()),
            tool_call: None,
            waited: false,
        }
    }

    pub async fn command(&self, command: &str) -> ShellInfo {
        self.create(self.input(command)).await
    }

    pub async fn create(&self, input: CreateInput) -> ShellInfo {
        self.shell.create(input, |_| {}).await.expect("a shell command starts")
    }

    pub async fn finish(&self, command: &str) -> ShellInfo {
        let info = self.command(command).await;
        self.shell.wait(&info.id.0).await.expect("the command ends")
    }

    pub fn drain_events(&self) -> Vec<(String, Value)> {
        lock(&self.events).try_iter().collect()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.shell.shutdown();
    }
}

fn environment() -> Vec<(String, String)> {
    std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .collect()
}

#[tokio::test]
async fn runs_a_command_and_reports_its_captured_output_and_exit() {
    let harness = Harness::new();
    let info = harness.command("printf 'hello\\n'").await;
    let done = harness.shell.wait(&info.id.0).await.expect("the command ends");
    assert_eq!(done.status, ShellInfoStatus::Exited);
    assert_eq!(done.exit, Some(0.0));
    assert_eq!(done.signal, None);
    assert!(done.time.completed.is_some());

    let result = harness.shell.result(done.clone(), None).await;
    let capture = result.capture.expect("the output file is known");
    assert_eq!(capture.output, "hello\n");
    assert!(!capture.truncated);

    let page = harness.shell.output(&info.id.0, None, None).expect("the command is known");
    assert_eq!(page.output, "hello\n");
    assert_eq!(page.cursor, page.size);
    assert!(!page.truncated);
    assert_eq!(
        PathBuf::from(&info.file),
        harness.data.join("shell").join(&harness.project).join(format!("{}.out", info.id.0))
    );

    let events: Vec<String> = harness.drain_events().into_iter().map(|(name, _)| name).collect();
    assert_eq!(events, vec!["shell.created".to_owned(), "shell.exited".to_owned()]);
}

#[tokio::test]
async fn reports_exit_codes_and_signals() {
    let harness = Harness::new();
    let failed = harness.finish("exit 3").await;
    assert_eq!(failed.status, ShellInfoStatus::Exited);
    assert_eq!(failed.exit, Some(3.0));
    assert_eq!(failed.signal, None);

    let signalled = harness.finish("kill -TERM $$").await;
    assert_eq!(signalled.status, ShellInfoStatus::Exited);
    assert_eq!(signalled.exit, None);
    assert_eq!(signalled.signal.as_deref(), Some("SIGTERM"));
}

#[tokio::test]
async fn times_out_a_command_and_clears_a_replaced_timeout() {
    let harness = Harness::new();
    let mut timed = harness.input("sleep 60");
    timed.timeout = Some(100);
    let info = harness.create(timed).await;
    let done = harness.shell.wait(&info.id.0).await.expect("the command ends");
    assert_eq!(done.status, ShellInfoStatus::Timeout);
    assert!(done.time.completed.is_some());
    let events = harness.drain_events();
    let exits: Vec<_> = events.iter().filter(|(name, _)| name == "shell.exited").collect();
    assert_eq!(exits.len(), 1);
    assert_eq!(exits[0].1["status"], "timeout");

    let mut cleared = harness.input("sleep 60");
    cleared.timeout = Some(300);
    let info = harness.create(cleared).await;
    harness.shell.timeout(&info.id.0, 0).expect("the command is known");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(harness.shell.get(&info.id.0).expect("the command is known").status, ShellInfoStatus::Running);
    harness.shell.stop(&info.id.0, StopReason::Killed).expect("the command is known");
    let done = harness.shell.wait(&info.id.0).await.expect("the command ends");
    assert_eq!(done.status, ShellInfoStatus::Killed);
}

#[tokio::test]
async fn stop_escalates_to_sigkill_after_the_grace_period() {
    let harness = Harness::new();
    let info = harness.command("trap '' TERM; while :; do :; done").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    harness.shell.stop(&info.id.0, StopReason::Killed).expect("the command is known");
    let done = harness.shell.wait(&info.id.0).await.expect("the command ends");
    assert_eq!(done.status, ShellInfoStatus::Killed);
    assert_eq!(done.signal.as_deref(), Some("SIGKILL"));
}

#[tokio::test]
async fn pages_output_across_utf8_boundaries() {
    let harness = Harness::new();
    let info = harness.command("printf 'a\\303\\251b'").await;
    harness.shell.wait(&info.id.0).await.expect("the command ends");

    let middle = harness.shell.output(&info.id.0, Some(1), Some(1)).expect("the command is known");
    assert_eq!(middle.output, "é");
    assert_eq!(middle.cursor, 3.0);
    assert_eq!(middle.size, 4.0);

    let last = harness.shell.output(&info.id.0, Some(3), Some(1)).expect("the command is known");
    assert_eq!(last.output, "b");
    assert_eq!(last.cursor, 4.0);

    let caught_up = harness.shell.output(&info.id.0, Some(4), Some(1)).expect("the command is known");
    assert_eq!(caught_up.output, "");
    assert_eq!(caught_up.cursor, 4.0);

    let zero = harness.shell.output(&info.id.0, Some(0), Some(0)).expect("the command is known");
    assert_eq!(zero.output, "");
    assert_eq!(zero.cursor, 0.0);
}

#[tokio::test]
async fn captures_output_larger_than_the_pipe_buffer_and_pages_it() {
    let harness = Harness::new();
    let info = harness.finish("i=1; while [ $i -le 20000 ]; do echo $i; i=$((i+1)); done").await;
    let result = harness.shell.result(info.clone(), None).await;
    assert!(result.capture.expect("the output file is known").truncated);

    let mut collected = String::new();
    let mut cursor = None;
    loop {
        let page = harness.shell.output(&info.id.0, cursor, None).expect("the command is known");
        collected.push_str(&page.output);
        if page.cursor >= page.size {
            break;
        }
        cursor = Some(page.cursor as u64);
    }
    assert!(collected.starts_with("1\n2\n"));
    assert!(collected.ends_with("20000\n"));
    assert_eq!(collected.lines().count(), 20_000);
}

#[tokio::test]
async fn holds_back_an_incomplete_character_while_the_command_runs() {
    let harness = Harness::new();
    let info = harness.command("printf '\\303'; sleep 1; printf '\\251'").await;
    let partial = poll_output(&harness.shell, &info.id.0, |page| page.size >= 1.0).await;
    assert_eq!(partial.output, "");
    assert_eq!(partial.cursor, 0.0);
    harness.shell.wait(&info.id.0).await.expect("the command ends");
    let full = harness.shell.output(&info.id.0, None, None).expect("the command is known");
    assert_eq!(full.output, "é");
    assert_eq!(full.size, 2.0);
}

async fn poll_output(
    shell: &Shell,
    id: &str,
    ready: impl Fn(&ShellOutputResponseData) -> bool,
) -> ShellOutputResponseData {
    for _ in 0..200 {
        let page = shell.output(id, None, None).expect("the command is known");
        if ready(&page) {
            return page;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("the command did not write its first byte in time");
}

#[tokio::test]
async fn removing_a_running_command_reports_killed_once_and_forgets_it() {
    let harness = Harness::new();
    let info = harness.command("sleep 60").await;
    harness.drain_events();
    harness.shell.remove(&info.id.0).expect("the command is known");
    assert!(matches!(harness.shell.get(&info.id.0), Err(ShellError::NotFound { .. })));
    assert!(matches!(harness.shell.wait(&info.id.0).await, Err(ShellError::NotFound { .. })));

    let result = harness.shell.result(info.clone(), None).await;
    assert_eq!(result.info.status, ShellInfoStatus::Killed);
    assert!(result.capture.is_none());
    assert!(!Path::new(&info.file).exists());

    let events = harness.drain_events();
    let names: Vec<&str> = events.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, vec!["shell.exited", "shell.deleted"]);
    assert_eq!(events[0].1["status"], "killed");
    assert_eq!(events[1].1["id"], info.id.0.as_str());
}

#[tokio::test]
async fn lists_only_running_commands() {
    let harness = Harness::new();
    let info = harness.command("sleep 60").await;
    assert!(harness.shell.list().iter().any(|candidate| candidate.id == info.id));
    harness.shell.stop(&info.id.0, StopReason::Killed).expect("the command is known");
    harness.shell.wait(&info.id.0).await.expect("the command ends");
    assert!(harness.shell.list().is_empty());
}

#[tokio::test]
async fn result_returns_the_output_so_far_when_the_wait_expires() {
    let harness = Harness::new();
    let info = harness.command("sleep 60").await;
    let result = harness.shell.result(info.clone(), Some(Duration::from_millis(50))).await;
    assert_eq!(result.info.status, ShellInfoStatus::Running);
    assert!(result.capture.is_some());
    harness.shell.stop(&info.id.0, StopReason::Killed).expect("the command is known");
    harness.shell.wait(&info.id.0).await.expect("the command ends");
}

#[tokio::test]
async fn before_rewrites_the_invocation_and_keeps_the_tool_call() {
    let harness = Harness::new();
    std::fs::create_dir_all(harness.data.parent().expect("a parent").join("subdir")).expect("a subdirectory");
    let call = ToolCall {
        session_id: "ses_1".to_owned(),
        assistant_message_id: "msg_1".to_owned(),
        call_id: "call_1".to_owned(),
    };
    let mut input = harness.input("printf 'original'");
    input.tool_call = Some(call.clone());
    let mut seen = Vec::new();
    let info = harness
        .shell
        .create(input, |invocation| {
            seen = invocation.env.clone();
            invocation.command = "printf 'changed'".to_owned();
            invocation.cwd = "subdir".to_owned();
            invocation.timeout = 0;
        })
        .await
        .expect("a shell command starts");
    assert_eq!(PathBuf::from(&info.cwd), harness.data.parent().expect("a parent").join("subdir"));
    assert!(seen.iter().any(|(name, value)| name == "TERM" && value == "xterm-256color"));
    assert!(seen.iter().any(|(name, value)| name == "OPENCODE_TERMINAL" && value == "1"));

    let done = harness.shell.wait(&info.id.0).await.expect("the command ends");
    let result = harness.shell.result(done, None).await;
    assert_eq!(result.capture.expect("the output file is known").output, "changed");
    assert_eq!(harness.shell.by_tool_call(&call).map(|found| found.id), Some(info.id));
    assert!(!harness.shell.handoff_prepared());
}
