use std::ffi::OsStr;
use std::fs;
use std::path::Path;

use opencode_util::path::{Flavor, windows_path};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub path: String,
    pub name: String,
    pub acceptable: bool,
}

pub fn list(shells: &Path, path: Option<&OsStr>, bin: &Path, gitbash: Option<&Path>) -> Vec<Item> {
    let gitbash = resolve_gitbash(gitbash, path, bin);
    candidates(shells, path, bin, gitbash.as_deref())
        .into_iter()
        .filter(|shell| executable(shell, gitbash.as_deref(), path, bin).is_some())
        .map(|shell| info(&shell, gitbash.as_deref(), path, bin))
        .collect()
}

fn name(file: &str) -> String {
    if !cfg!(windows) {
        return Flavor::Posix.basename(file).to_lowercase();
    }
    let normalized = windows_path(Flavor::Windows, file);
    stem(Flavor::Windows.basename(normalized.as_ref())).to_lowercase()
}

fn stem(base: &str) -> &str {
    match base.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() && base != "." && base != ".." => stem,
        _ => base,
    }
}

fn compatible(file: &str) -> bool {
    !matches!(name(file).as_str(), "fish" | "nu")
}

fn full(file: &str, gitbash: Option<&str>, path: Option<&OsStr>, bin: &Path) -> String {
    if !cfg!(windows) {
        return file.to_owned();
    }
    let shell = windows_path(Flavor::Windows, file).into_owned();
    if !dirname_is_dot(&shell) {
        if shell.starts_with('/') && name(&shell) == "bash" {
            return gitbash.map(str::to_owned).unwrap_or(shell);
        }
        return shell;
    }
    if name(&shell) == "bash" {
        return gitbash.map(str::to_owned).or_else(|| find_executable(&shell, path, bin)).unwrap_or(shell);
    }
    find_executable(&shell, path, bin).unwrap_or(shell)
}

fn dirname_is_dot(file: &str) -> bool {
    let root = Flavor::Windows.root(file);
    if !root.is_empty() {
        return false;
    }
    let trimmed = file.trim_end_matches(['/', '\\']);
    match trimmed.rfind(['/', '\\']) {
        None => true,
        Some(1) => trimmed.starts_with('.'),
        Some(_) => false,
    }
}

fn rooted(file: &str) -> bool {
    Flavor::NATIVE.is_absolute(&windows_path(Flavor::NATIVE, file))
}

fn executable(file: &str, gitbash: Option<&str>, path: Option<&OsStr>, bin: &Path) -> Option<String> {
    let shell = full(file, gitbash, path, bin);
    if rooted(&shell) {
        return launchable(&shell).then_some(shell);
    }
    find_executable(&shell, path, bin)
}

fn info(file: &str, gitbash: Option<&str>, path: Option<&OsStr>, bin: &Path) -> Item {
    let item = full(file, gitbash, path, bin);
    let n = name(&item);
    let acceptable = compatible(&item);
    Item { path: item.clone(), name: if executable(&n, gitbash, path, bin).is_some() { n } else { item }, acceptable }
}

fn regular_file(file: &str) -> bool {
    fs::metadata(file).is_ok_and(|meta| meta.is_file())
}

fn launchable(file: &str) -> bool {
    #[cfg(windows)]
    {
        if app_execution_alias(file) {
            return true;
        }
    }
    regular_file(file)
}

#[cfg(windows)]
fn app_execution_alias(file: &str) -> bool {
    fs::symlink_metadata(file).is_ok_and(|meta| meta.file_type().is_symlink()) && fs::exists(file).unwrap_or(false)
}

fn search_path(path: Option<&OsStr>, bin: &Path) -> String {
    let base = path.map(|value| value.to_string_lossy()).unwrap_or_default();
    let bin = bin.to_string_lossy();
    match (base.is_empty(), bin.is_empty()) {
        (true, _) => bin.into_owned(),
        (false, true) => base.into_owned(),
        (false, false) => format!("{base}{}{bin}", delimiter()),
    }
}

fn delimiter() -> char {
    if cfg!(windows) {
        return ';';
    }
    ':'
}

fn has_slash(cmd: &str) -> bool {
    if cfg!(windows) {
        return cmd.contains(['/', '\\']);
    }
    cmd.contains('/')
}

fn find_executable(cmd: &str, path: Option<&OsStr>, bin: &Path) -> Option<String> {
    let search = search_path(path, bin);
    if has_slash(cmd) {
        return locate(cmd, "");
    }
    search_dirs(&search).iter().find_map(|dir| locate(cmd, dir)).or_else(|| alias_fallback(cmd, &search))
}

fn search_dirs(search: &str) -> Vec<String> {
    #[cfg(windows)]
    let cwd = std::env::current_dir().ok().map(|cwd| cwd.to_string_lossy().into_owned());
    #[cfg(not(windows))]
    let cwd = None::<String>;
    cwd.into_iter().chain(search.split(delimiter()).map(str::to_owned)).collect()
}

#[cfg(windows)]
fn is_exe_name(cmd: &str) -> bool {
    let base = Flavor::Windows.basename(cmd);
    let Some((stem, extension)) = base.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty() && extension.eq_ignore_ascii_case("exe")
}

#[cfg(windows)]
fn alias_fallback(cmd: &str, search: &str) -> Option<String> {
    if search.is_empty() || !dirname_is_dot(cmd) {
        return None;
    }
    let name = if is_exe_name(cmd) { cmd.to_owned() } else { format!("{cmd}.exe") };
    search
        .split(delimiter())
        .filter(|dir| !dir.is_empty())
        .map(|dir| {
            let dir = dir.strip_prefix('"').and_then(|dir| dir.strip_suffix('"')).unwrap_or(dir);
            Flavor::Windows.join(&[dir, &name])
        })
        .find(|file| app_execution_alias(file))
}

#[cfg(not(windows))]
fn alias_fallback(_cmd: &str, _search: &str) -> Option<String> {
    None
}

fn locate(cmd: &str, dir: &str) -> Option<String> {
    let dir = dir.strip_prefix('"').and_then(|dir| dir.strip_suffix('"')).unwrap_or(dir);
    let relative = dir.is_empty() && (cmd.starts_with("./") || (cfg!(windows) && cmd.starts_with(".\\")));
    let prefix = if relative { &cmd[..2] } else { "" };
    let base = format!("{prefix}{}", Flavor::NATIVE.join(&[dir, cmd]));
    candidates_for(&base, cmd).into_iter().find(|file| executable_file(file))
}

#[cfg(unix)]
fn candidates_for(base: &str, _cmd: &str) -> Vec<String> {
    vec![base.to_owned()]
}

#[cfg(windows)]
fn candidates_for(base: &str, cmd: &str) -> Vec<String> {
    let mut extensions = vec![".EXE", ".exe", ".CMD", ".cmd", ".BAT", ".bat", ".COM", ".com"];
    if cmd.contains('.') {
        extensions.insert(0, "");
    }
    extensions.into_iter().map(|extension| format!("{base}{extension}")).collect()
}

#[cfg(unix)]
fn executable_file(file: &str) -> bool {
    use rustix::fs::{Access, access};
    regular_file(file) && access(Path::new(file), Access::EXEC_OK).is_ok()
}

#[cfg(windows)]
fn executable_file(file: &str) -> bool {
    regular_file(file)
}

#[cfg(windows)]
fn candidates(_shells: &Path, path: Option<&OsStr>, bin: &Path, gitbash: Option<&str>) -> Vec<String> {
    win(path, bin, gitbash)
}

#[cfg(not(windows))]
fn candidates(shells: &Path, _path: Option<&OsStr>, _bin: &Path, _gitbash: Option<&str>) -> Vec<String> {
    unix(shells)
}

fn unix(shells: &Path) -> Vec<String> {
    let bytes = fs::read(shells).unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes);
    if text.is_empty() {
        return ["/bin/bash", "/bin/zsh", "/bin/sh"].map(str::to_owned).to_vec();
    }
    text.split('\n').filter(|line| !line.trim().is_empty() && !line.starts_with('#')).fold(
        Vec::new(),
        |mut unique: Vec<String>, line| {
            let line = line.to_owned();
            if !unique.contains(&line) {
                unique.push(line);
            }
            unique
        },
    )
}

#[cfg(windows)]
fn win(path: Option<&OsStr>, bin: &Path, gitbash: Option<&str>) -> Vec<String> {
    let comspec = std::env::var_os("COMSPEC")
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "cmd.exe".to_owned());
    [
        find_executable("pwsh", path, bin),
        find_executable("powershell", path, bin),
        gitbash.map(str::to_owned),
        Some(comspec),
    ]
    .into_iter()
    .flatten()
    .map(|shell| full(&shell, gitbash, path, bin))
    .fold(Vec::new(), |mut unique: Vec<String>, shell| {
        if !unique.contains(&shell) {
            unique.push(shell);
        }
        unique
    })
}

#[cfg(windows)]
fn resolve_gitbash(config: Option<&Path>, path: Option<&OsStr>, bin: &Path) -> Option<String> {
    if let Some(config) = config {
        return Some(config.to_string_lossy().into_owned());
    }
    let git = find_executable("git", path, bin)?;
    let file = Flavor::Windows.join(&[&git, "..", "..", "bin", "bash.exe"]);
    fs::metadata(&file).ok().filter(|meta| meta.len() != 0).map(|_| file)
}

#[cfg(not(windows))]
fn resolve_gitbash(_config: Option<&Path>, _path: Option<&OsStr>, _bin: &Path) -> Option<String> {
    None
}
