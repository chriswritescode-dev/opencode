use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Running,
    Exited,
    Timeout,
    Killed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Info {
    pub id: String,
    pub status: Status,
    pub command: String,
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Capture {
    pub output: String,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub killed: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShellResult {
    pub info: Info,
    #[serde(default)]
    pub capture: Option<Capture>,
}

impl ShellResult {
    pub fn new(info: Info, capture: Option<Capture>) -> Self {
        Self { info, capture }
    }
}

const MISSING: &str = "Shell command output is no longer available.";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unavailable {
    pub output: &'static str,
    pub cursor: usize,
    pub size: usize,
    pub truncated: bool,
}

pub const UNAVAILABLE: Unavailable =
    Unavailable { output: MISSING, cursor: MISSING.len(), size: MISSING.len(), truncated: false };

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Output {
    pub output: String,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub killed: Option<String>,
}

pub fn output(result: &ShellResult) -> Output {
    Output {
        output: result.capture.as_ref().map_or_else(|| UNAVAILABLE.output.to_owned(), |capture| capture.output.clone()),
        truncated: result.capture.as_ref().is_some_and(|capture| capture.truncated),
        exit: result.info.exit,
        signal: result.info.signal.clone(),
        timeout: (result.info.status == Status::Timeout).then_some(true),
        killed: result.capture.as_ref().and_then(|capture| capture.killed.clone()),
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Interrupted {
    pub output: String,
    pub notice: String,
    pub message: String,
}

pub fn interrupted(result: &ShellResult) -> Interrupted {
    let stopping = if result.info.status == Status::Running { "; it is still being stopped" } else { "" };
    let stopped = format!("Command interrupted by the user (shell ID: {}){stopping}.", result.info.id);
    let (output, notice) = match &result.capture {
        None => (UNAVAILABLE.output.to_owned(), stopped),
        Some(capture) if capture.truncated => (capture.output.clone(), stopped),
        Some(capture) if capture.output == "(no output)" => {
            (String::new(), format!("[no output; output file: {}]\n\n{stopped}", result.info.file))
        }
        Some(capture) => (
            capture.output.strip_suffix('\n').unwrap_or(&capture.output).to_owned(),
            format!("[full output saved to {}]\n\n{stopped}", result.info.file),
        ),
    };
    let message = if output.is_empty() { notice.clone() } else { format!("{output}\n\n{notice}") };
    Interrupted { output, notice, message }
}

pub fn notice(output: &Output, timeout: Option<f64>) -> Option<String> {
    if let Some(killed) = &output.killed {
        return Some(killed.clone());
    }
    if output.timeout == Some(true) {
        let after = match timeout {
            Some(value) if value != 0.0 => format!(" after {} seconds", value / 1_000.0),
            _ => String::new(),
        };
        return Some(format!(
            "Command timed out{after}. If it needs longer and isn't waiting for input, rerun with a larger timeout.\nExit code: {}",
            exit_code(output)
        ));
    }
    if output.signal.is_some() || (output.exit.is_some() && output.exit != Some(0)) {
        return Some(format!("Exit code: {}", exit_code(output)));
    }
    None
}

fn exit_code(output: &Output) -> String {
    if let Some(exit) = output.exit {
        return exit.to_string();
    }
    match output.signal.as_deref().and_then(signal_number) {
        None => "unknown".to_owned(),
        Some(number) => format!("{} ({})", 128 + number, output.signal.as_deref().unwrap_or_default()),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Metadata {
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub killed: Option<bool>,
}

pub fn metadata(output: &Output) -> Metadata {
    Metadata {
        truncated: output.truncated,
        exit: output.exit,
        signal: output.signal.clone(),
        timeout: output.timeout,
        killed: output.killed.as_ref().map(|_| true),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Completed,
    Cancelled,
    Error,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationInput {
    #[serde(rename = "shellID")]
    pub shell_id: String,
    #[serde(default, rename = "jobID", skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    pub command: String,
    pub state: State,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Output>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationMetadata {
    pub source: String,
    #[serde(rename = "shellID")]
    pub shell_id: String,
    #[serde(default, rename = "jobID", skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    pub state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub killed: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    pub text: String,
    pub metadata: NotificationMetadata,
}

pub fn notification(input: NotificationInput) -> Notification {
    let attributes = format!(
        "kind=\"shell\" id=\"{}\" state=\"{}\" command=\"{}\"",
        input.job_id.as_deref().unwrap_or(&input.shell_id),
        state_name(input.state),
        input.command
    );
    let text = format!("<background-result {attributes}>\n{}\n</background-result>", input.text);
    let captured = input.output.map(|output| metadata(&output));
    Notification {
        text,
        metadata: NotificationMetadata {
            source: "shell".to_owned(),
            shell_id: input.shell_id,
            job_id: input.job_id,
            state: input.state,
            truncated: captured.as_ref().map(|metadata| metadata.truncated),
            exit: captured.as_ref().and_then(|metadata| metadata.exit),
            signal: captured.as_ref().and_then(|metadata| metadata.signal.clone()),
            timeout: captured.as_ref().and_then(|metadata| metadata.timeout),
            killed: captured.as_ref().and_then(|metadata| metadata.killed),
        },
    }
}

pub fn user_notification(result: &ShellResult) -> Notification {
    let captured = output(result);
    let status = if result.info.status == Status::Killed {
        Some("Cancelled".to_owned())
    } else if captured.exit == Some(0) {
        None
    } else {
        Some(notice(&captured, None).unwrap_or_else(|| "Exit code: unknown".to_owned()))
    };
    let text = [Some(captured.output.as_str()), status.as_deref()]
        .into_iter()
        .flatten()
        .filter(|item| !item.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let message = notification(NotificationInput {
        shell_id: result.info.id.clone(),
        job_id: None,
        command: result.info.command.clone(),
        state: if result.info.status == Status::Killed { State::Cancelled } else { State::Completed },
        text,
        output: Some(captured),
    });
    Notification {
        text: format!("The following shell command was executed by the user:\n{}", message.text),
        metadata: message.metadata,
    }
}

fn state_name(state: State) -> &'static str {
    match state {
        State::Completed => "completed",
        State::Cancelled => "cancelled",
        State::Error => "error",
    }
}

fn signal_number(signal: &str) -> Option<i32> {
    let name = signal.strip_prefix("SIG")?;
    let number = if cfg!(target_os = "macos") {
        match name {
            "HUP" => 1,
            "INT" => 2,
            "QUIT" => 3,
            "ILL" => 4,
            "TRAP" => 5,
            "ABRT" => 6,
            "EMT" => 7,
            "FPE" => 8,
            "KILL" => 9,
            "BUS" => 10,
            "SEGV" => 11,
            "SYS" => 12,
            "PIPE" => 13,
            "ALRM" => 14,
            "TERM" => 15,
            "URG" => 16,
            "STOP" => 17,
            "TSTP" => 18,
            "CONT" => 19,
            "CHLD" => 20,
            "TTIN" => 21,
            "TTOU" => 22,
            "IO" => 23,
            "XCPU" => 24,
            "XFSZ" => 25,
            "VTALRM" => 26,
            "PROF" => 27,
            "WINCH" => 28,
            "INFO" => 29,
            "USR1" => 30,
            "USR2" => 31,
            _ => return None,
        }
    } else if cfg!(windows) {
        match name {
            "HUP" => 1,
            "INT" => 2,
            "QUIT" => 3,
            "ILL" => 4,
            "TRAP" => 5,
            "ABRT" => 22,
            "BUS" => 10,
            "FPE" => 8,
            "KILL" => 9,
            "SEGV" => 11,
            "TERM" => 15,
            "BREAK" => 21,
            "WINCH" => 28,
            _ => return None,
        }
    } else {
        match name {
            "HUP" => 1,
            "INT" => 2,
            "QUIT" => 3,
            "ILL" => 4,
            "TRAP" => 5,
            "ABRT" => 6,
            "BUS" => 7,
            "FPE" => 8,
            "KILL" => 9,
            "USR1" => 10,
            "SEGV" => 11,
            "USR2" => 12,
            "PIPE" => 13,
            "ALRM" => 14,
            "TERM" => 15,
            "STKFLT" => 16,
            "CHLD" => 17,
            "CONT" => 18,
            "STOP" => 19,
            "TSTP" => 20,
            "TTIN" => 21,
            "TTOU" => 22,
            "URG" => 23,
            "XCPU" => 24,
            "XFSZ" => 25,
            "VTALRM" => 26,
            "PROF" => 27,
            "WINCH" => 28,
            "IO" => 29,
            "PWR" => 30,
            "SYS" => 31,
            _ => return None,
        }
    };
    Some(number)
}
