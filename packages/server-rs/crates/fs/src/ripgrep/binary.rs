use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use opencode_config::paths::GlobalPaths;
use tokio::sync::OnceCell;

const VERSION: &str = "15.1.0";
#[cfg(windows)]
const WINDOWS_EXTENSIONS: &str = ".COM;.EXE;.BAT;.CMD";

type Vars = Box<dyn Fn(&str) -> Option<OsString> + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Extension {
    TarGz,
    Zip,
}

impl Extension {
    fn text(self) -> &'static str {
        match self {
            Self::TarGz => "tar.gz",
            Self::Zip => "zip",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Release {
    pub platform: String,
    pub extension: Extension,
    pub sha256: String,
    pub url: String,
}

#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct BinaryError {
    pub message: String,
}

fn failed(message: impl Into<String>) -> BinaryError {
    BinaryError { message: message.into() }
}

pub fn host_platform() -> Option<&'static str> {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "linux") => Some("x86_64-unknown-linux-musl"),
        ("aarch64", "linux") => Some("aarch64-unknown-linux-gnu"),
        ("x86_64", "macos") => Some("x86_64-apple-darwin"),
        ("aarch64", "macos") => Some("aarch64-apple-darwin"),
        ("x86_64", "windows") => Some("x86_64-pc-windows-msvc"),
        ("aarch64", "windows") => Some("aarch64-pc-windows-msvc"),
        ("x86", "windows") => Some("i686-pc-windows-msvc"),
        _ => None,
    }
}

pub fn releases() -> Vec<Release> {
    [
        ("aarch64-apple-darwin", Extension::TarGz, "378e973289176ca0c6054054ee7f631a065874a352bf43f0fa60ef079b6ba715"),
        (
            "aarch64-unknown-linux-gnu",
            Extension::TarGz,
            "2b661c6ef508e902f388e9098d9c4c5aca72c87b55922d94abdba830b4dc885e",
        ),
        ("x86_64-apple-darwin", Extension::TarGz, "64811cb24e77cac3057d6c40b63ac9becf9082eedd54ca411b475b755d334882"),
        (
            "x86_64-unknown-linux-musl",
            Extension::TarGz,
            "1c9297be4a084eea7ecaedf93eb03d058d6faae29bbc57ecdaf5063921491599",
        ),
        ("aarch64-pc-windows-msvc", Extension::Zip, "00d931fb5237c9696ca49308818edb76d8eb6fc132761cb2a1bd616b2df02f8e"),
        ("i686-pc-windows-msvc", Extension::Zip, "725be85a1e8f92878a548f40ee4f6df64bc93b809586462b3c6d884e1de1e83a"),
        ("x86_64-pc-windows-msvc", Extension::Zip, "124510b94b6baa3380d051fdf4650eaa80a302c876d611e9dba0b2e18d87493a"),
    ]
    .into_iter()
    .map(|(platform, extension, sha256)| Release {
        platform: platform.to_string(),
        extension,
        sha256: sha256.to_string(),
        url: format!(
            "https://github.com/BurntSushi/ripgrep/releases/download/{VERSION}/ripgrep-{VERSION}-{platform}.{}",
            extension.text()
        ),
    })
    .collect()
}

pub fn verify(bytes: &[u8], filename: &str, expected: &str) -> Result<(), BinaryError> {
    let actual = opencode_util::hash::sha256_hex(bytes);
    if actual != expected {
        return Err(failed(format!(
            "ripgrep download {filename} failed SHA-256 verification: expected {expected}, got {actual}"
        )));
    }
    Ok(())
}

pub struct Binary {
    bin: PathBuf,
    vars: Vars,
    releases: Vec<Release>,
    filepath: OnceCell<PathBuf>,
}

impl Binary {
    pub fn new(
        paths: GlobalPaths,
        vars: impl Fn(&str) -> Option<OsString> + Send + Sync + 'static,
        releases: Vec<Release>,
    ) -> Self {
        Self { bin: paths.bin, vars: Box::new(vars), releases, filepath: OnceCell::new() }
    }

    pub async fn filepath(&self) -> Result<PathBuf, BinaryError> {
        self.filepath.get_or_try_init(|| self.resolve()).await.cloned()
    }

    async fn resolve(&self) -> Result<PathBuf, BinaryError> {
        let name = if cfg!(windows) { "rg.exe" } else { "rg" };
        if let Some(system) = self.which(name)
            && tokio::fs::metadata(&system).await.is_ok_and(|metadata| metadata.is_file())
        {
            return Ok(system);
        }

        let target = self.bin.join(name);
        if tokio::fs::metadata(&target).await.is_ok_and(|metadata| metadata.is_file()) {
            return Ok(target);
        }

        let platform = host_platform().ok_or_else(|| {
            failed(format!("unsupported platform for ripgrep: {}-{}", std::env::consts::ARCH, std::env::consts::OS))
        })?;
        let release = self
            .releases
            .iter()
            .find(|release| release.platform == platform)
            .ok_or_else(|| failed(format!("unsupported platform for ripgrep: {platform}")))?;
        self.download(release, name).await
    }

    fn var(&self, name: &str) -> Option<String> {
        (self.vars)(name).filter(|value| !value.is_empty()).map(|value| value.to_string_lossy().into_owned())
    }

    fn which(&self, command: &str) -> Option<PathBuf> {
        let path = self.var("PATH").or_else(|| self.var("Path")).unwrap_or_default();
        let bin = self.bin.to_string_lossy();
        let full = if path.is_empty() { bin.into_owned() } else { format!("{path}{}{bin}", delimiter()) };
        #[cfg(windows)]
        {
            let extensions = self.var("PATHEXT").or_else(|| self.var("PathExt"));
            which_in(&full, extensions.as_deref(), command)
        }
        #[cfg(not(windows))]
        {
            which_in(&full, command)
        }
    }

    async fn download(&self, release: &Release, name: &str) -> Result<PathBuf, BinaryError> {
        let filename = format!("ripgrep-{VERSION}-{}.{}", release.platform, release.extension.text());
        let archive = self.bin.join(&filename);
        let target = self.bin.join(name);

        tokio::fs::create_dir_all(&self.bin)
            .await
            .map_err(|error| failed(format!("failed to create {}: {error}", self.bin.display())))?;

        let response = opencode_config::http::client()
            .get(&release.url)
            .send()
            .await
            .map_err(|error| failed(format!("failed to download ripgrep from {}: {error}", release.url)))?;
        if !response.status().is_success() {
            return Err(failed(format!("failed to download ripgrep from {}: HTTP {}", release.url, response.status())));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|error| failed(format!("failed to download ripgrep from {}: {error}", release.url)))?;
        if bytes.is_empty() {
            return Err(failed(format!("failed to download ripgrep from {}", release.url)));
        }
        verify(&bytes, &filename, &release.sha256)?;

        tokio::fs::write(&archive, &bytes)
            .await
            .map_err(|error| failed(format!("failed to write {}: {error}", archive.display())))?;
        self.extract(&archive, release, &target).await?;
        let _ = tokio::fs::remove_file(&archive).await;
        Ok(target)
    }

    async fn extract(&self, archive: &Path, release: &Release, target: &Path) -> Result<(), BinaryError> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = self.bin.join(format!("ripgrep-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst)));
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|error| failed(format!("failed to create {}: {error}", dir.display())))?;
        let result = self.extract_into(archive, release, target, &dir).await;
        let _ = tokio::fs::remove_dir_all(&dir).await;
        result
    }

    async fn extract_into(
        &self,
        archive: &Path,
        release: &Release,
        target: &Path,
        dir: &Path,
    ) -> Result<(), BinaryError> {
        let output = match release.extension {
            Extension::TarGz => {
                let archive = archive.to_string_lossy();
                run(Path::new("tar"), &["-xzf", &archive, "-C", &dir.to_string_lossy()], dir).await?
            }
            Extension::Zip => {
                let system = self.var("SystemRoot").unwrap_or_else(|| "C:\\Windows".to_string());
                let tar = Path::new(&system).join("System32").join("tar.exe");
                let name = archive
                    .file_name()
                    .ok_or_else(|| failed(format!("invalid ripgrep archive path: {}", archive.display())))?;
                let relative = format!("../{}", name.to_string_lossy());
                run(&tar, &["-xf", &relative], dir).await?
            }
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let message = match (stderr.trim(), stdout.trim()) {
                ("", "") => format!(
                    "ripgrep extraction failed with code {}",
                    output.status.code().map_or_else(|| "unknown".to_string(), |code| code.to_string())
                ),
                (stderr, _) if !stderr.is_empty() => stderr.to_string(),
                (_, stdout) => stdout.to_string(),
            };
            return Err(failed(message));
        }

        let name = if cfg!(windows) { "rg.exe" } else { "rg" };
        let extracted = dir.join(format!("ripgrep-{VERSION}-{}", release.platform)).join(name);
        if !tokio::fs::metadata(&extracted).await.is_ok_and(|metadata| metadata.is_file()) {
            return Err(failed(format!("ripgrep archive did not contain executable: {}", extracted.display())));
        }

        tokio::fs::copy(&extracted, target)
            .await
            .map_err(|error| failed(format!("failed to copy ripgrep executable to {}: {error}", target.display())))?;
        #[cfg(unix)]
        tokio::fs::set_permissions(target, std::fs::Permissions::from_mode(0o755))
            .await
            .map_err(|error| failed(format!("failed to make {} executable: {error}", target.display())))?;
        Ok(())
    }
}

async fn run(program: &Path, args: &[&str], cwd: &Path) -> Result<std::process::Output, BinaryError> {
    tokio::process::Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|error| failed(format!("failed to run {}: {error}", program.display())))
}

pub(crate) fn delimiter() -> char {
    if cfg!(windows) { ';' } else { ':' }
}

pub(crate) fn which(command: &str, path: &str, extensions: Option<&str>) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        which_in(path, extensions, command)
    }
    #[cfg(not(windows))]
    {
        let _ = extensions;
        which_in(path, command)
    }
}

#[cfg(windows)]
fn which_in(path: &str, extensions: Option<&str>, command: &str) -> Option<PathBuf> {
    let extensions = extensions.filter(|value| !value.is_empty()).unwrap_or(WINDOWS_EXTENSIONS);
    let dirs = path.split(';').map(unquote).filter(|dir| !dir.is_empty()).collect::<Vec<_>>();
    let executable = dirs.iter().flat_map(|dir| {
        extensions
            .split(';')
            .filter(|extension| !extension.is_empty())
            .map(move |extension| Path::new(dir).join(format!("{command}{extension}")))
    });
    executable.into_iter().find(|candidate| is_executable_file(candidate)).or_else(|| {
        let name =
            if command.to_lowercase().ends_with(".exe") { command.to_string() } else { format!("{command}.exe") };
        dirs.iter().map(|dir| Path::new(dir).join(&name)).find(|candidate| is_app_execution_alias(candidate))
    })
}

#[cfg(not(windows))]
fn which_in(path: &str, command: &str) -> Option<PathBuf> {
    path.split(':')
        .filter(|dir| !dir.is_empty())
        .map(|dir| Path::new(dir).join(command))
        .find(|candidate| is_executable_file(candidate))
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
