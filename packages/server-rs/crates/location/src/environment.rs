use futures_util::future::BoxFuture;

pub mod local;
pub mod memory;
pub mod unavailable;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileType {
    File,
    Directory,
    Symlink,
    Other,
}

impl std::fmt::Display for FileType {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            FileType::File => "file",
            FileType::Directory => "directory",
            FileType::Symlink => "symlink",
            FileType::Other => "other",
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FileInfo {
    pub kind: FileType,
    pub size: u64,
    pub mtime_ms: f64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub kind: FileType,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileRange {
    pub offset: u64,
    pub length: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FileRead {
    pub info: FileInfo,
    pub bytes: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum EnvironmentError {
    #[error("Not found: {path}")]
    NotFound { path: String },
    #[error("Wrong kind at {path}: {actual}")]
    WrongKind { path: String, actual: FileType },
    #[error("Failed at {path}: {cause}")]
    Failed { path: String, cause: String },
}

impl EnvironmentError {
    pub fn not_found(path: impl Into<String>) -> Self {
        EnvironmentError::NotFound { path: path.into() }
    }

    pub fn failed(path: impl Into<String>, cause: impl std::fmt::Display) -> Self {
        EnvironmentError::Failed { path: path.into(), cause: cause.to_string() }
    }
}

pub trait Files: Send + Sync {
    fn read<'a>(&'a self, path: &'a str, range: Option<FileRange>)
    -> BoxFuture<'a, Result<FileRead, EnvironmentError>>;

    fn write<'a>(&'a self, path: &'a str, bytes: &'a [u8]) -> BoxFuture<'a, Result<(), EnvironmentError>>;

    fn stat<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<FileInfo, EnvironmentError>>;

    fn list<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Vec<DirEntry>, EnvironmentError>>;

    fn remove<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), EnvironmentError>>;

    fn rename<'a>(&'a self, from: &'a str, to: &'a str) -> BoxFuture<'a, Result<(), EnvironmentError>>;

    fn mkdir<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), EnvironmentError>>;

    fn append<'a>(&'a self, path: &'a str, bytes: &'a [u8]) -> BoxFuture<'a, Result<(), EnvironmentError>>;
}

pub async fn type_following(files: &dyn Files, path: &str) -> Result<FileType, EnvironmentError> {
    let info = files.stat(path).await?;
    if info.kind != FileType::Symlink {
        return Ok(info.kind);
    }
    match files.read(path, Some(FileRange { offset: 0, length: 0 })).await {
        Ok(read) => Ok(read.info.kind),
        Err(EnvironmentError::WrongKind { actual, .. }) => Ok(actual),
        Err(error) => Err(error),
    }
}

pub async fn exists(files: &dyn Files, path: &str) -> Result<bool, EnvironmentError> {
    match type_following(files, path).await {
        Ok(_) => Ok(true),
        Err(EnvironmentError::NotFound { .. }) => Ok(false),
        Err(error) => Err(error),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandOutput {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub trait Spawner: Send + Sync {
    fn spawn<'a>(&'a self, command: &'a CommandSpec) -> BoxFuture<'a, Result<CommandOutput, EnvironmentError>>;
}

pub const LOCAL_ID: &str = "local";

pub trait Environment: Send + Sync {
    fn id(&self) -> &str;
    fn files(&self) -> &dyn Files;
    fn spawner(&self) -> &dyn Spawner;
    fn hosts_server(&self) -> bool;
    fn searchable(&self) -> bool;
}
