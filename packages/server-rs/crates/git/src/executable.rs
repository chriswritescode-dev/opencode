use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use crate::Vars;

const DARWIN_SHIM: &str = "/usr/bin/git";
const XCODE_SELECT_LINK: &str = "/var/db/xcode_select_link";
const DEFAULT_DEVELOPER_DIRS: [&str; 2] =
    ["/Applications/Xcode.app/Contents/Developer", "/Library/Developer/CommandLineTools"];
#[cfg(windows)]
const WINDOWS_EXTENSIONS: &str = ".COM;.EXE;.BAT;.CMD";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Linux,
    Darwin,
    Windows,
    Other,
}

impl Platform {
    pub fn current() -> Platform {
        match std::env::consts::OS {
            "windows" => Platform::Windows,
            "macos" => Platform::Darwin,
            "linux" => Platform::Linux,
            _ => Platform::Other,
        }
    }
}

pub fn darwin_developer_dirs(developer_dir: Option<&str>, read_link: impl Fn(&str) -> Option<String>) -> Vec<String> {
    let developer_dir = developer_dir.map(str::trim).filter(|dir| !dir.is_empty());
    if let Some(dir) = developer_dir {
        return vec![dir.to_string()];
    }
    let mut dirs = read_link(XCODE_SELECT_LINK).into_iter().collect::<Vec<_>>();
    dirs.extend(DEFAULT_DEVELOPER_DIRS.map(str::to_string));
    dirs
}

pub fn resolve_git_executable(
    platform: Platform,
    resolved: Option<&str>,
    developer_dirs: &[String],
    exists: impl Fn(&str) -> bool,
) -> String {
    match platform {
        Platform::Windows => resolved.map_or_else(|| "git".to_string(), windows_absolute),
        Platform::Darwin => darwin_git_executable(resolved, developer_dirs, exists),
        Platform::Linux | Platform::Other => "git".to_string(),
    }
}

pub(crate) fn git_executable(vars: &Vars) -> OsString {
    let platform = Platform::current();
    if matches!(platform, Platform::Linux | Platform::Other) {
        return OsString::from("git");
    }
    let resolved = which(vars, "git");
    if platform == Platform::Windows {
        return OsString::from(resolve_git_executable(platform, resolved.as_deref(), &[], |_| false));
    }
    let developer_dir = vars("DEVELOPER_DIR").map(|value| value.to_string_lossy().into_owned());
    let dirs = darwin_developer_dirs(developer_dir.as_deref(), read_xcode_select_link);
    OsString::from(resolve_git_executable(platform, resolved.as_deref(), &dirs, |candidate| {
        Path::new(candidate).exists()
    }))
}

fn windows_absolute(value: &str) -> String {
    std::path::absolute(value).unwrap_or_else(|_| PathBuf::from(value)).to_string_lossy().into_owned()
}

fn darwin_git_executable(resolved: Option<&str>, developer_dirs: &[String], exists: impl Fn(&str) -> bool) -> String {
    let Some(resolved) = resolved else {
        return "git".to_string();
    };
    let full = posix_absolute(resolved);
    if full != DARWIN_SHIM {
        return full;
    }
    developer_dirs
        .iter()
        .filter(|dir| dir.starts_with('/'))
        .map(|dir| format!("{}/usr/bin/git", dir.trim_end_matches('/')))
        .find(|candidate| candidate != DARWIN_SHIM && exists(candidate))
        .unwrap_or_else(|| "git".to_string())
}

fn posix_absolute(value: &str) -> String {
    let resolved = match std::env::current_dir() {
        Ok(current) if !value.starts_with('/') => format!("{}/{}", current.to_string_lossy(), value),
        _ => value.to_string(),
    };
    let mut segments: Vec<&str> = Vec::new();
    for segment in resolved.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            segment => segments.push(segment),
        }
    }
    format!("/{}", segments.join("/"))
}

fn read_xcode_select_link(link: &str) -> Option<String> {
    let path = std::fs::canonicalize(link).ok()?;
    Some(path.to_string_lossy().into_owned())
}

fn which(vars: &Vars, command: &str) -> Option<String> {
    let path =
        vars("PATH").or_else(|| vars("Path")).map(|value| value.to_string_lossy().into_owned()).unwrap_or_default();
    let extensions = vars("PATHEXT").or_else(|| vars("PathExt")).map(|value| value.to_string_lossy().into_owned());
    which_in(&path, extensions.as_deref(), command)
}

#[cfg(windows)]
fn which_in(path: &str, extensions: Option<&str>, command: &str) -> Option<String> {
    let extensions = extensions.filter(|value| !value.is_empty()).unwrap_or(WINDOWS_EXTENSIONS);
    let dirs = path.split(';').map(unquote).filter(|dir| !dir.is_empty()).collect::<Vec<_>>();
    let executable = dirs
        .iter()
        .flat_map(|dir| {
            extensions
                .split(';')
                .filter(|extension| !extension.is_empty())
                .map(move |extension| Path::new(dir).join(format!("{command}{extension}")))
        })
        .find(|candidate| is_executable_file(candidate));
    let found = executable.or_else(|| {
        let name =
            if command.to_lowercase().ends_with(".exe") { command.to_string() } else { format!("{command}.exe") };
        dirs.iter().map(|dir| Path::new(dir).join(&name)).find(|candidate| is_app_execution_alias(candidate))
    });
    found.map(|candidate| candidate.to_string_lossy().into_owned())
}

#[cfg(unix)]
fn which_in(path: &str, _extensions: Option<&str>, command: &str) -> Option<String> {
    path.split(':')
        .filter(|dir| !dir.is_empty())
        .map(|dir| Path::new(dir).join(command))
        .find(|candidate| is_executable_file(candidate))
        .map(|candidate| candidate.to_string_lossy().into_owned())
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(windows)]
fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path).map(|metadata| metadata.is_file()).unwrap_or(false)
}

#[cfg(windows)]
fn is_app_execution_alias(path: &Path) -> bool {
    std::fs::symlink_metadata(path).map(|metadata| metadata.file_type().is_symlink()).unwrap_or(false)
        && std::fs::exists(path).unwrap_or(false)
}

#[cfg(windows)]
fn unquote(dir: &str) -> &str {
    dir.strip_prefix('"').and_then(|value| value.strip_suffix('"')).unwrap_or(dir)
}
