pub mod binary;

use std::process::Stdio;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::{ChildStderr, ChildStdout};
use tokio::sync::watch;

use crate::filesystem::{Entry, EntryKind};

pub use binary::{Binary, BinaryError};

pub const DEFAULT_SEARCH_LIMIT: usize = 100;
pub const DEFAULT_SEARCH_TIMEOUT_MS: u64 = 30_000;

const ERROR_BYTES: usize = 8 * 1024;
const MAX_SUBMATCHES: usize = 100;
const MAX_LINE_CHARS: usize = 2_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Submatch {
    pub text: String,
    pub start: u64,
    pub end: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Match {
    pub entry: Entry,
    pub line: u64,
    pub offset: u64,
    pub text: String,
    pub submatches: Vec<Submatch>,
}

pub struct GlobInput {
    pub cwd: String,
    pub pattern: String,
    pub limit: usize,
    pub hidden: bool,
    pub follow: bool,
    pub signal: Option<watch::Receiver<bool>>,
}

impl Default for GlobInput {
    fn default() -> Self {
        Self {
            cwd: String::new(),
            pattern: String::new(),
            limit: DEFAULT_SEARCH_LIMIT,
            hidden: false,
            follow: false,
            signal: None,
        }
    }
}

pub type EntryHook = Arc<dyn Fn(&Entry) + Send + Sync>;

pub struct FindInput {
    pub cwd: String,
    pub pattern: String,
    pub limit: usize,
    pub exclude: Vec<String>,
    pub hidden: bool,
    pub follow: bool,
    pub signal: Option<watch::Receiver<bool>>,
    pub on_entry: Option<EntryHook>,
}

impl Default for FindInput {
    fn default() -> Self {
        Self {
            cwd: String::new(),
            pattern: String::new(),
            limit: DEFAULT_SEARCH_LIMIT,
            exclude: Vec::new(),
            hidden: false,
            follow: false,
            signal: None,
            on_entry: None,
        }
    }
}

pub struct GrepInput {
    pub cwd: String,
    pub pattern: String,
    pub file: Option<String>,
    pub include: Option<String>,
    pub literal: bool,
    pub case_sensitive: Option<bool>,
    pub limit: usize,
    pub signal: Option<watch::Receiver<bool>>,
}

impl Default for GrepInput {
    fn default() -> Self {
        Self {
            cwd: String::new(),
            pattern: String::new(),
            file: None,
            include: None,
            literal: false,
            case_sensitive: None,
            limit: DEFAULT_SEARCH_LIMIT,
            signal: None,
        }
    }
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum Error {
    #[error("{message}")]
    Failed { message: String },
    #[error("{message}")]
    InvalidGlob { message: String },
    #[error("{message}")]
    InvalidPattern { pattern: String, message: String },
}

fn failed(message: impl Into<String>) -> Error {
    Error::Failed { message: message.into() }
}

pub struct Ripgrep {
    binary: Binary,
}

struct Spawn {
    cwd: String,
    args: Vec<String>,
    limit: usize,
    signal: Option<watch::Receiver<bool>>,
    pattern: Option<String>,
}

impl Ripgrep {
    pub fn new(binary: Binary) -> Self {
        Self { binary }
    }

    pub async fn glob(&self, input: GlobInput) -> Result<Vec<Entry>, Error> {
        let args = glob_args(&input);
        self.rows(
            Spawn { cwd: input.cwd, args, limit: input.limit, signal: input.signal, pattern: None },
            |line| Ok(Some(file_entry(line))),
            |_| {},
        )
        .await
        .map_err(pattern_failure)
    }

    pub async fn find(&self, input: FindInput) -> Result<Vec<Entry>, Error> {
        let args = find_args(&input);
        let on_entry = input.on_entry.clone();
        self.rows(
            Spawn { cwd: input.cwd, args, limit: input.limit, signal: input.signal, pattern: None },
            |line| Ok(Some(file_entry(line))),
            move |entry| {
                if let Some(callback) = &on_entry {
                    callback(entry);
                }
            },
        )
        .await
        .map_err(pattern_failure)
    }

    pub async fn grep(&self, input: GrepInput) -> Result<Vec<Match>, Error> {
        let args = grep_args(&input);
        self.rows(
            Spawn { cwd: input.cwd, args, limit: input.limit, signal: input.signal, pattern: Some(input.pattern) },
            parse_match,
            |_| {},
        )
        .await
    }

    async fn rows<T, P, H>(&self, spawn: Spawn, parse: P, on_item: H) -> Result<Vec<T>, Error>
    where
        T: Send + 'static,
        P: Fn(&str) -> Result<Option<T>, Error> + Send + 'static,
        H: Fn(&T) + Send + 'static,
    {
        let Spawn { cwd, args, limit, signal, pattern } = spawn;
        let program = self.binary.filepath().await.map_err(|error| Error::Failed { message: error.message })?;
        let mut command = tokio::process::Command::new(program);
        command
            .args(&args)
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|error| failed(format!("ripgrep execution failed: {error}")))?;
        let stdout = child.stdout.take().ok_or_else(|| failed("ripgrep stdout was not captured"))?;
        let stderr = child.stderr.take().ok_or_else(|| failed("ripgrep stderr was not captured"))?;
        let stderr = tokio::spawn(read_stderr(stderr));
        let mut reader = tokio::spawn(read_rows(stdout, limit, parse, on_item));

        enum Outcome<T> {
            Rows(Result<Result<Rows<T>, Error>, tokio::task::JoinError>),
            Cancelled,
        }

        let outcome = tokio::select! {
            result = &mut reader => Outcome::Rows(result),
            () = wait_cancel(signal) => Outcome::Cancelled,
        };

        let rows = match outcome {
            Outcome::Cancelled => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                reader.abort();
                let _ = reader.await;
                let _ = stderr.await;
                return Err(failed("ripgrep execution cancelled"));
            }
            Outcome::Rows(result) => match result {
                Ok(Ok(rows)) => rows,
                Ok(Err(error)) => {
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                    let _ = stderr.await;
                    return Err(error);
                }
                Err(error) => {
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                    let _ = stderr.await;
                    return Err(failed(format!("ripgrep execution failed: {error}")));
                }
            },
        };

        if rows.truncated {
            let _ = child.start_kill();
            let _ = child.wait().await;
            let _ = stderr.await;
            return Ok(rows.rows);
        }

        let status = child.wait().await.map_err(|error| failed(format!("ripgrep execution failed: {error}")))?;
        let stderr = stderr.await.map_err(|error| failed(format!("ripgrep execution failed: {error}")))?;
        let stderr = String::from_utf8_lossy(&stderr);
        match status.code() {
            Some(0) => Ok(rows.rows),
            Some(1) => Ok(Vec::new()),
            Some(2) => {
                if let Some(message) = invalid_glob(&stderr) {
                    return Err(Error::InvalidGlob { message });
                }
                if let Some(pattern) = pattern.as_deref()
                    && is_invalid_pattern(&stderr)
                {
                    return Err(Error::InvalidPattern {
                        pattern: pattern.to_string(),
                        message: stderr.trim().to_string(),
                    });
                }
                Ok(rows.rows)
            }
            Some(code) => {
                let message = stderr.trim();
                Err(failed(if message.is_empty() {
                    format!("ripgrep failed with code {code}")
                } else {
                    message.to_string()
                }))
            }
            None => {
                let message = stderr.trim();
                Err(failed(if message.is_empty() {
                    "ripgrep execution failed".to_string()
                } else {
                    message.to_string()
                }))
            }
        }
    }
}

struct Rows<T> {
    rows: Vec<T>,
    truncated: bool,
}

async fn read_rows<T, P, H>(stdout: ChildStdout, limit: usize, parse: P, on_item: H) -> Result<Rows<T>, Error>
where
    P: Fn(&str) -> Result<Option<T>, Error>,
    H: Fn(&T),
{
    let mut reader = BufReader::new(stdout);
    let mut rows = Vec::new();
    let mut observed = 0usize;
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        let count = reader
            .read_until(b'\n', &mut buffer)
            .await
            .map_err(|error| failed(format!("ripgrep execution failed: {error}")))?;
        if count == 0 {
            return Ok(Rows { rows, truncated: false });
        }
        while matches!(buffer.last(), Some(b'\n' | b'\r')) {
            buffer.pop();
        }
        if buffer.is_empty() {
            continue;
        }
        let line = String::from_utf8_lossy(&buffer);
        let Some(row) = parse(&line)? else {
            continue;
        };
        if observed < limit {
            on_item(&row);
        }
        observed += 1;
        rows.push(row);
        if rows.len() > limit {
            rows.truncate(limit);
            return Ok(Rows { rows, truncated: true });
        }
    }
}

async fn read_stderr(mut stderr: ChildStderr) -> Vec<u8> {
    let mut captured = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match stderr.read(&mut chunk).await {
            Ok(0) | Err(_) => return captured,
            Ok(count) => {
                let room = ERROR_BYTES.saturating_sub(captured.len());
                if room > 0 {
                    captured.extend_from_slice(&chunk[..count.min(room)]);
                }
            }
        }
    }
}

async fn wait_cancel(mut signal: Option<watch::Receiver<bool>>) {
    let Some(receiver) = signal.as_mut() else {
        return std::future::pending().await;
    };
    loop {
        if *receiver.borrow() {
            return;
        }
        if receiver.changed().await.is_err() {
            return std::future::pending().await;
        }
    }
}

fn glob_args(input: &GlobInput) -> Vec<String> {
    let mut args = vec!["--no-config".to_string(), "--files".to_string()];
    if input.hidden {
        args.push("--hidden".to_string());
    }
    if input.follow {
        args.push("--follow".to_string());
    }
    args.push(format!("--glob={}", input.pattern));
    if !input.hidden {
        args.push("--glob=!**/.*".to_string());
    }
    args.push("--glob=!**/.git/**".to_string());
    args.push(".".to_string());
    args
}

fn find_args(input: &FindInput) -> Vec<String> {
    let mut args = vec!["--no-config".to_string(), "--files".to_string()];
    if input.hidden {
        args.push("--hidden".to_string());
    }
    if input.follow {
        args.push("--follow".to_string());
    }
    if input.pattern != "*" {
        args.push(format!("--glob={}", input.pattern));
    }
    args.extend(input.exclude.iter().map(|pattern| format!("--glob=!{pattern}")));
    args.push("--glob=!**/.git/**".to_string());
    args.push(".".to_string());
    args
}

fn grep_args(input: &GrepInput) -> Vec<String> {
    let mut args =
        vec!["--no-config".to_string(), "--json".to_string(), "--hidden".to_string(), "--no-messages".to_string()];
    if input.literal {
        args.push("--fixed-strings".to_string());
    }
    if input.case_sensitive == Some(false) {
        args.push("--ignore-case".to_string());
    }
    if let Some(include) = &input.include {
        args.push(format!("--glob={include}"));
    }
    args.push("--glob=!**/.git/**".to_string());
    args.push("--".to_string());
    args.push(input.pattern.clone());
    args.push(input.file.clone().unwrap_or_else(|| ".".to_string()));
    args
}

fn pattern_failure(error: Error) -> Error {
    match error {
        Error::InvalidPattern { message, .. } => Error::Failed { message },
        other => other,
    }
}

fn file_entry(line: &str) -> Entry {
    Entry { path: normalize_path(line), kind: EntryKind::File }
}

fn parse_match(line: &str) -> Result<Option<Match>, Error> {
    let json: Value =
        serde_json::from_str(line).map_err(|error| failed(format!("Invalid ripgrep JSON output: {error}")))?;
    if json.get("type").and_then(Value::as_str) != Some("match") {
        return Ok(None);
    }
    let data = json.get("data").ok_or_else(|| failed("Invalid ripgrep match output: missing data"))?;
    let lines = raw_text(data.get("lines").ok_or_else(|| failed("Invalid ripgrep match output: missing lines"))?)?;
    let length = lines.encode_utf16().count();
    let text = if length > MAX_LINE_CHARS { format!("{}...", head_utf16(&lines, MAX_LINE_CHARS)) } else { lines };
    let submatches = data
        .get("submatches")
        .and_then(Value::as_array)
        .ok_or_else(|| failed("Invalid ripgrep match output: missing submatches"))?;
    let submatches = submatches
        .iter()
        .take(MAX_SUBMATCHES)
        .map(|submatch| {
            Ok(Submatch {
                text: raw_text(
                    submatch.get("match").ok_or_else(|| failed("Invalid ripgrep match output: missing submatch"))?,
                )?,
                start: number(submatch.get("start"))?,
                end: number(submatch.get("end"))?,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    Ok(Some(Match {
        entry: Entry {
            path: normalize_path(&raw_text(
                data.get("path").ok_or_else(|| failed("Invalid ripgrep match output: missing path"))?,
            )?),
            kind: EntryKind::File,
        },
        line: number(data.get("line_number"))?,
        offset: number(data.get("absolute_offset"))?,
        text,
        submatches,
    }))
}

fn raw_text(data: &Value) -> Result<String, Error> {
    if let Some(text) = data.get("text").and_then(Value::as_str) {
        return Ok(text.to_string());
    }
    let bytes = data
        .get("bytes")
        .and_then(Value::as_str)
        .ok_or_else(|| failed("Invalid ripgrep match output: raw data is not text or bytes"))?;
    let decoded = STANDARD.decode(bytes).map_err(|error| failed(format!("Invalid ripgrep match output: {error}")))?;
    Ok(String::from_utf8_lossy(&decoded).into_owned())
}

fn number(value: Option<&Value>) -> Result<u64, Error> {
    value.and_then(Value::as_u64).ok_or_else(|| failed("Invalid ripgrep match output: expected a number"))
}

fn normalize_path(value: &str) -> String {
    let mut rest = value;
    while let Some(stripped) = rest.strip_prefix("./").or_else(|| rest.strip_prefix(".\\")) {
        rest = stripped;
    }
    rest.trim_start_matches(['/', '\\']).replace('\\', "/")
}

fn invalid_glob(stderr: &str) -> Option<String> {
    let rest = stderr.strip_prefix("rg: error parsing glob ").or_else(|| stderr.strip_prefix("error parsing glob "))?;
    Some(rest.trim_end().to_string())
}

fn is_invalid_pattern(stderr: &str) -> bool {
    stderr.contains("regex parse error") || stderr.contains("error parsing regex")
}

fn head_utf16(text: &str, limit: usize) -> &str {
    let mut units = 0;
    for (index, character) in text.char_indices() {
        let next = units + character.len_utf16();
        if next > limit {
            return &text[..index];
        }
        units = next;
    }
    text
}
