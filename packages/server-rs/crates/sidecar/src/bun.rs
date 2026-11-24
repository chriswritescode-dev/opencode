//! Finding the runtime the sidecar runs on: `OPENCODE_BUN`, an opencode binary run as the Bun it embeds, or `bun`
//! itself. Nothing from the project directory, or from a relative `PATH` entry, is ever taken.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::time::timeout;

use crate::config::NAMES;

/// The oldest Bun the sidecar runs on: an opencode binary whose own Bun is older is not used.
pub const OLDEST_BUN: (u64, u64, u64) = (1, 4, 0);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Runtime {
    pub bun: PathBuf,
    /// `bun` is an opencode binary, which runs as Bun with `BUN_BE_BUN=1`.
    pub opencode: bool,
    /// The version the runtime reported as Bun's, when it was asked.
    pub version: Option<String>,
}

/// Bun for the sidecar, over the variables `vars` reads, for a server working for `directory`: `OPENCODE_BUN` (an
/// absolute path only), else the TypeScript `opencode` beside this executable, else the first `opencode` on `PATH`
/// that answers as a new enough Bun, else `bun` on `PATH`.
pub async fn find(vars: &impl Fn(&str) -> Option<OsString>, directory: &Path) -> Option<Runtime> {
    let home = vars("HOME").map(PathBuf::from);
    let path = vars("PATH").unwrap_or_default();

    if let Some(bun) = vars("OPENCODE_BUN").map(PathBuf::from).filter(|bun| bun.is_absolute()) {
        return Some(Runtime { bun, opencode: false, version: None });
    }

    if let Some(bundled) = opencode_util::bundle::server()
        && outside(&bundled, directory, home.as_deref())
        && let Some(runtime) = opencode(&bundled, vars).await
    {
        return Some(runtime);
    }

    for binary in found_in("opencode", &path, directory, home.as_deref()) {
        if let Some(runtime) = opencode(&binary, vars).await {
            return Some(runtime);
        }
    }

    let bun = find_in("bun", &path, directory, home.as_deref())?;
    let version = bun_version(&bun, vars).await;
    Some(Runtime { bun, opencode: false, version })
}

/// `binary`, an opencode build, run as the Bun it embeds (`BUN_BE_BUN=1`), when that Bun says its version and it is
/// [`OLDEST_BUN`] or newer. Its real path is what is asked and run: `opencode` on the path may be a `mise` shim, a
/// link to `mise`, which then answers for itself and is not taken.
async fn opencode(binary: &Path, vars: &impl Fn(&str) -> Option<OsString>) -> Option<Runtime> {
    let bun = std::fs::canonicalize(binary).ok()?;
    let version = bun_version(&bun, vars).await?;
    if semver(&version)? < OLDEST_BUN {
        return None;
    }
    Some(Runtime { bun, opencode: true, version: Some(version) })
}

/// The version `binary` prints for `Bun.version`, when it prints one version alone within a second. It runs as the
/// sidecar does, apart from any project: in the binary's directory, with the allowlisted environment.
async fn bun_version(binary: &Path, vars: &impl Fn(&str) -> Option<OsString>) -> Option<String> {
    let mut command = tokio::process::Command::new(binary);
    command
        .args(["-e", "process.stdout.write(Bun.version)"])
        .current_dir(binary.parent()?)
        .env_clear()
        .env("BUN_BE_BUN", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    for name in NAMES {
        if let Some(value) = vars(name) {
            command.env(name, value);
        }
    }
    let output = timeout(Duration::from_secs(1), command.output()).await.ok()?.ok()?;
    let version = String::from_utf8(output.stdout).ok()?;
    let version = version.trim();
    (output.status.success() && semver(version).is_some()).then(|| version.to_owned())
}

/// `major.minor.patch` of a version alone (a `-` or `+` suffix allowed), as Bun prints its own.
fn semver(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.split(['-', '+']).next()?.split('.').map(|part| part.parse::<u64>().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

/// `name` in the entries of `path`, as a server working for `directory` finds it: only in an absolute entry, as a
/// relative or empty one is the working directory, and not inside `directory` (unless that is the home directory or a
/// filesystem root, which is no project): a project's own `bun` would run as soon as the server opened it.
pub fn find_in(name: &str, path: &OsStr, directory: &Path, home: Option<&Path>) -> Option<PathBuf> {
    found_in(name, path, directory, home).into_iter().next()
}

/// Every `name` [`find_in`] would take, in the order of `path`.
fn found_in(name: &str, path: &OsStr, directory: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    std::env::split_paths(path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(&file))
        .filter(|path| path.is_file() && outside(path, directory, home))
        .collect()
}

/// Whether `path` is outside the project in `directory`; a home directory or a filesystem root is no project.
fn outside(path: &Path, directory: &Path, home: Option<&Path>) -> bool {
    let real = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let project = real(directory);
    let no_project = project.parent().is_none() || Some(&project) == home.map(real).as_ref();
    no_project || !real(path).starts_with(&project)
}
