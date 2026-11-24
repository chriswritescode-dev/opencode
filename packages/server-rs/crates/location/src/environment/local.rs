use std::path::{Path, PathBuf};

use futures_util::future::BoxFuture;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use super::{
    CommandOutput, CommandSpec, DirEntry, Environment, EnvironmentError, FileInfo, FileRange, FileRead, FileType,
    Files, LOCAL_ID, Spawner,
};

#[derive(Clone, Copy, Debug, Default)]
pub struct LocalFiles;

impl LocalFiles {
    pub fn new() -> Self {
        LocalFiles
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LocalSpawner;

impl LocalSpawner {
    pub fn new() -> Self {
        LocalSpawner
    }
}

pub struct LocalEnvironment {
    files: LocalFiles,
    spawner: LocalSpawner,
}

impl LocalEnvironment {
    pub fn new() -> Self {
        LocalEnvironment { files: LocalFiles::new(), spawner: LocalSpawner::new() }
    }
}

impl Default for LocalEnvironment {
    fn default() -> Self {
        LocalEnvironment::new()
    }
}

impl Environment for LocalEnvironment {
    fn id(&self) -> &str {
        LOCAL_ID
    }

    fn files(&self) -> &dyn Files {
        &self.files
    }

    fn spawner(&self) -> &dyn Spawner {
        &self.spawner
    }

    fn hosts_server(&self) -> bool {
        true
    }

    fn searchable(&self) -> bool {
        true
    }
}

impl Files for LocalFiles {
    fn read<'a>(
        &'a self,
        path: &'a str,
        range: Option<FileRange>,
    ) -> BoxFuture<'a, Result<FileRead, EnvironmentError>> {
        Box::pin(async move {
            let info = stat(path, true).await?;
            if info.kind != FileType::File {
                return Err(EnvironmentError::WrongKind { path: path.to_string(), actual: info.kind });
            }
            let Some(range) = range else {
                let bytes = tokio::fs::read(path).await.map_err(|error| map_io(path, error))?;
                return Ok(FileRead { info, bytes });
            };
            let mut file = tokio::fs::File::open(path).await.map_err(|error| map_io(path, error))?;
            file.seek(std::io::SeekFrom::Start(range.offset)).await.map_err(|error| map_io(path, error))?;
            let mut reader = file.take(range.length);
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).await.map_err(|error| map_io(path, error))?;
            Ok(FileRead { info, bytes })
        })
    }

    fn write<'a>(&'a self, path: &'a str, bytes: &'a [u8]) -> BoxFuture<'a, Result<(), EnvironmentError>> {
        Box::pin(async move {
            if let Some(parent) = Path::new(path).parent() {
                tokio::fs::create_dir_all(parent).await.map_err(|error| map_io(path, error))?;
            }
            tokio::fs::write(path, bytes).await.map_err(|error| map_io(path, error))
        })
    }

    fn stat<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<FileInfo, EnvironmentError>> {
        Box::pin(async move { stat(path, false).await })
    }

    fn list<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Vec<DirEntry>, EnvironmentError>> {
        Box::pin(async move {
            let info = stat(path, true).await?;
            if info.kind != FileType::Directory {
                return Err(EnvironmentError::WrongKind { path: path.to_string(), actual: info.kind });
            }
            let mut directory = tokio::fs::read_dir(path).await.map_err(|error| map_io(path, error))?;
            let mut entries = Vec::new();
            while let Some(entry) = directory.next_entry().await.map_err(|error| map_io(path, error))? {
                let kind = entry.file_type().await.map_err(|error| map_io(path, error))?;
                entries.push(DirEntry { name: entry.file_name().to_string_lossy().into_owned(), kind: kind_of(kind) });
            }
            Ok(entries)
        })
    }

    fn remove<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), EnvironmentError>> {
        Box::pin(async move {
            let metadata = match tokio::fs::symlink_metadata(path).await {
                Ok(metadata) => metadata,
                Err(error) if is_missing(&error) => return Ok(()),
                Err(error) => return Err(EnvironmentError::failed(path, error)),
            };
            let result = if metadata.file_type().is_dir() {
                tokio::fs::remove_dir_all(path).await
            } else {
                tokio::fs::remove_file(path).await
            };
            result.map_err(|error| map_io(path, error))
        })
    }

    fn rename<'a>(&'a self, from: &'a str, to: &'a str) -> BoxFuture<'a, Result<(), EnvironmentError>> {
        Box::pin(async move {
            stat(from, false).await?;
            let destination: PathBuf = match stat(to, false).await {
                Ok(info) if info.kind == FileType::Directory => {
                    let name = Path::new(from).file_name().unwrap_or(std::ffi::OsStr::new(""));
                    Path::new(to).join(name)
                }
                Ok(_) => Path::new(to).to_path_buf(),
                Err(EnvironmentError::NotFound { .. }) => Path::new(to).to_path_buf(),
                Err(error) => return Err(error),
            };
            tokio::fs::rename(from, destination).await.map_err(|error| map_io(from, error))
        })
    }

    fn mkdir<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), EnvironmentError>> {
        Box::pin(async move { tokio::fs::create_dir_all(path).await.map_err(|error| map_io(path, error)) })
    }

    fn append<'a>(&'a self, path: &'a str, bytes: &'a [u8]) -> BoxFuture<'a, Result<(), EnvironmentError>> {
        Box::pin(async move {
            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .await
                .map_err(|error| map_io(path, error))?;
            file.write_all(bytes).await.map_err(|error| map_io(path, error))
        })
    }
}

impl Spawner for LocalSpawner {
    fn spawn<'a>(&'a self, command: &'a CommandSpec) -> BoxFuture<'a, Result<CommandOutput, EnvironmentError>> {
        Box::pin(async move {
            let mut process = tokio::process::Command::new(&command.program);
            process.args(&command.args);
            if let Some(cwd) = &command.cwd {
                process.current_dir(cwd);
            }
            process.envs(command.env.iter().map(|(key, value)| (key.as_str(), value.as_str())));
            let output =
                process.output().await.map_err(|error| EnvironmentError::failed(command.program.clone(), error))?;
            Ok(CommandOutput {
                status: output.status.code().unwrap_or(-1),
                stdout: output.stdout,
                stderr: output.stderr,
            })
        })
    }
}

async fn stat(path: &str, follow: bool) -> Result<FileInfo, EnvironmentError> {
    let metadata = if follow { tokio::fs::metadata(path).await } else { tokio::fs::symlink_metadata(path).await };
    let metadata = metadata.map_err(|error| map_io(path, error))?;
    Ok(FileInfo { kind: kind_of(metadata.file_type()), size: metadata.len(), mtime_ms: mtime_ms(&metadata) })
}

fn kind_of(kind: std::fs::FileType) -> FileType {
    if kind.is_file() {
        return FileType::File;
    }
    if kind.is_dir() {
        return FileType::Directory;
    }
    if kind.is_symlink() {
        return FileType::Symlink;
    }
    FileType::Other
}

fn mtime_ms(metadata: &std::fs::Metadata) -> f64 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0)
}

fn is_missing(error: &std::io::Error) -> bool {
    matches!(error.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory)
}

fn map_io(path: &str, error: std::io::Error) -> EnvironmentError {
    if is_missing(&error) {
        return EnvironmentError::not_found(path);
    }
    EnvironmentError::failed(path, error)
}
