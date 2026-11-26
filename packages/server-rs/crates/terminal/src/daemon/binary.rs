use std::path::{Path, PathBuf};

use crate::daemon::DaemonError;

const BINARY_NAME: &str = "opencode-pty";

pub fn resolve(env: &[(String, String)]) -> Result<PathBuf, DaemonError> {
    if let Some(configured) = lookup(env, "OPENCODE_PTY_BIN").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(configured));
    }
    let path = lookup(env, "PATH").unwrap_or_default();
    std::env::split_paths(Path::new(&path))
        .map(|directory| directory.join(BINARY_NAME))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| DaemonError::spawn(format!("{BINARY_NAME} was not found on PATH")))
}

fn lookup<'a>(env: &'a [(String, String)], name: &str) -> Option<&'a str> {
    env.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
}
