//! What the sidecar starts with: a snapshot of the server's variables, the directory it works for, where its private
//! files live, and the limits both sides keep.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::time::Duration;

use crate::bun::Runtime;

/// The variables the sidecar inherits: user paths, locale, terminal and proxy configuration. Node and Bun options can
/// preload code, and anything `OPENCODE_*` is the server's, such as its password.
pub(crate) const NAMES: &[&str] = &[
    "TERM",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "COLORTERM",
    "COLORFGBG",
    "NO_COLOR",
    "FORCE_COLOR",
    "TMUX",
    "TMUX_PANE",
    "STY",
    "TERMINAL_EMULATOR",
    "VTE_VERSION",
    "KONSOLE_VERSION",
    "ITERM_SESSION_ID",
    "ITERM_PROFILE",
    "WT_SESSION",
    "WT_PROFILE_ID",
    "WEZTERM_EXECUTABLE",
    "WEZTERM_PANE",
    "KITTY_WINDOW_ID",
    "KITTY_PID",
    "KITTY_INSTALLATION_DIR",
    "GHOSTTY_RESOURCES_DIR",
    "GHOSTTY_BIN_DIR",
    "ALACRITTY_WINDOW_ID",
    "CONEMUANSI",
    "WSL_DISTRO_NAME",
    "SSH_TTY",
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "TZ",
    "TMPDIR",
    "TEMP",
    "TMP",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "SYSTEMROOT",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "SYSTEMDRIVE",
];

/// Whether `name` may reach the sidecar.
pub fn inherited(name: &OsStr) -> bool {
    let name = name.to_string_lossy().to_ascii_uppercase();
    NAMES.contains(&name.as_str()) || name.starts_with("LC_") || name.starts_with("XDG_")
}

pub struct Config {
    /// The server's variables, snapshotted: the sidecar inherits the allowlisted ones and nothing else.
    pub vars: Vec<(OsString, OsString)>,
    /// The directory the sidecar works for, told as data. It is used only for Bun discovery, to refuse a runtime inside
    /// the project; the sidecar never starts Bun there.
    pub directory: PathBuf,
    /// Where the sidecar's private directory is made.
    pub tmp: PathBuf,
    pub limits: Limits,
    #[doc(hidden)]
    pub test_methods: bool,
    /// A runtime to use instead of finding one, for tests.
    #[doc(hidden)]
    pub bun: Option<Runtime>,
}

impl Config {
    pub fn new(
        vars: impl IntoIterator<Item = (impl Into<OsString>, impl Into<OsString>)>,
        directory: PathBuf,
        tmp: PathBuf,
    ) -> Self {
        Self {
            vars: vars.into_iter().map(|(name, value)| (name.into(), value.into())).collect(),
            directory,
            tmp,
            limits: Limits::default(),
            test_methods: false,
            bun: None,
        }
    }

    pub fn var(&self, name: &str) -> Option<OsString> {
        self.vars.iter().find(|(key, _)| key == OsStr::new(name)).map(|(_, value)| value.clone())
    }

    /// The runtime to start, either the one set for tests or the first [`crate::bun::find`] takes.
    pub async fn runtime(&self) -> Option<Runtime> {
        match &self.bun {
            Some(runtime) => Some(runtime.clone()),
            None => crate::bun::find(&|name| self.var(name), &self.directory).await,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_frame: usize,
    pub queued_bytes: usize,
    pub max_outgoing: usize,
    pub max_incoming: usize,
    pub window: u32,
    pub handshake: Duration,
    pub heartbeat: Duration,
    pub liveness: Duration,
    pub memory: u64,
    pub backoff_initial: Duration,
    pub backoff_max: Duration,
    pub crash_limit: usize,
    pub crash_window: Duration,
    pub grace: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_frame: 16 << 20,
            queued_bytes: 8 << 20,
            max_outgoing: 1024,
            max_incoming: 256,
            window: 16,
            handshake: Duration::from_secs(10),
            heartbeat: Duration::from_secs(5),
            liveness: Duration::from_secs(15),
            memory: 1 << 30,
            backoff_initial: Duration::from_millis(250),
            backoff_max: Duration::from_secs(30),
            crash_limit: 5,
            crash_window: Duration::from_secs(60),
            grace: Duration::from_millis(500),
        }
    }
}
