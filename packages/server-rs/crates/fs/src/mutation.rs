use std::io::ErrorKind;
use std::sync::OnceLock;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::FsError;
use crate::access::{self, Target};
use crate::bom;
use crate::filesystem::write_with_dirs;
use crate::mutex::{KeyedMutex, KeyedMutexGuard};
use crate::path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteResult {
    pub operation: &'static str,
    pub target: String,
    pub resource: String,
    pub existed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextWriteResult {
    pub operation: &'static str,
    pub target: String,
    pub resource: String,
    pub existed: bool,
    pub bom: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedText {
    pub text: String,
    pub bom: bool,
}

pub struct Mutation {
    locks: KeyedMutex<String>,
}

impl Default for Mutation {
    fn default() -> Self {
        Self::new()
    }
}

impl Mutation {
    pub fn new() -> Self {
        Self { locks: KeyedMutex::new() }
    }

    pub async fn with_lock<T>(&self, targets: &[String], run: impl std::future::Future<Output = T>) -> T {
        let keys = targets.iter().map(|target| path::resolve(target)).collect::<Vec<_>>();
        let _guards = transaction_locks().lock_all(keys).await;
        run.await
    }

    pub async fn write(&self, target: &Target, content: impl AsRef<[u8]>) -> Result<WriteResult, FsError> {
        let _guard = self.lock_target(target).await;
        let existed = tokio::fs::try_exists(&target.absolute).await.unwrap_or(false);
        write_with_dirs(&target.absolute, content.as_ref()).await?;
        Ok(write_result(target, existed))
    }

    pub async fn append(&self, target: &Target, content: impl AsRef<[u8]>) -> Result<WriteResult, FsError> {
        let _guard = self.lock_target(target).await;
        let existed = tokio::fs::try_exists(&target.absolute).await.unwrap_or(false);
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&target.absolute)
            .await
            .map_err(|error| io_error(&target.absolute, &error))?;
        file.write_all(content.as_ref()).await.map_err(|error| io_error(&target.absolute, &error))?;
        file.flush().await.map_err(|error| io_error(&target.absolute, &error))?;
        Ok(write_result(target, existed))
    }

    pub async fn write_text_preserving_bom(&self, target: &Target, content: &str) -> Result<TextWriteResult, FsError> {
        let _guard = self.lock_target(target).await;
        let (next_bom, next_text) = bom::split(content);
        let current = read_prefix(&target.absolute, 3).await?;
        let bom = current.as_deref().is_some_and(bom::has) || next_bom;
        write_with_dirs(&target.absolute, bom::join(next_text, bom).as_bytes()).await?;
        Ok(TextWriteResult {
            operation: "write",
            target: target.absolute.clone(),
            resource: target.resource.clone(),
            existed: current.is_some(),
            bom,
        })
    }

    pub async fn read_editable_text(&self, target: &Target) -> Result<DecodedText, FsError> {
        let bytes = tokio::fs::read(&target.absolute).await.map_err(|error| io_error(&target.absolute, &error))?;
        let text = std::str::from_utf8(&bytes).map_err(|_| FsError::InvalidUtf8 { path: target.absolute.clone() })?;
        let (bom, text) = bom::split(text);
        Ok(DecodedText { text: text.to_owned(), bom })
    }

    pub async fn read_text(&self, target: &Target) -> Result<DecodedText, FsError> {
        let bytes = tokio::fs::read(&target.absolute).await.map_err(|error| io_error(&target.absolute, &error))?;
        Ok(decode_lossy(&bytes))
    }

    pub async fn read_optional_text(&self, target: &Target) -> Result<Option<DecodedText>, FsError> {
        match tokio::fs::read(&target.absolute).await {
            Ok(bytes) => Ok(Some(decode_lossy(&bytes))),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(io_error(&target.absolute, &error)),
        }
    }

    pub async fn sync_text_bom(&self, target: &Target, bom: bool) -> Result<String, FsError> {
        let _guard = self.lock_target(target).await;
        let bytes = tokio::fs::read(&target.absolute).await.map_err(|error| io_error(&target.absolute, &error))?;
        let synced = bom::sync_bytes(&bytes, bom);
        if let Some(bytes) = synced.bytes {
            write_with_dirs(&target.absolute, &bytes).await?;
        }
        Ok(synced.text)
    }

    pub async fn lock_target(&self, target: &Target) -> KeyedMutexGuard<String> {
        self.locks.lock(access::key(target)).await
    }
}

fn transaction_locks() -> &'static KeyedMutex<String> {
    static LOCKS: OnceLock<KeyedMutex<String>> = OnceLock::new();
    LOCKS.get_or_init(KeyedMutex::new)
}

fn write_result(target: &Target, existed: bool) -> WriteResult {
    WriteResult { operation: "write", target: target.absolute.clone(), resource: target.resource.clone(), existed }
}

fn decode_lossy(bytes: &[u8]) -> DecodedText {
    let text = String::from_utf8_lossy(bytes).into_owned();
    let (bom, text) = bom::split(&text);
    DecodedText { text: text.to_owned(), bom }
}

async fn read_prefix(target: &str, length: u64) -> Result<Option<Vec<u8>>, FsError> {
    let file = match tokio::fs::File::open(target).await {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(target, &error)),
    };
    let mut bytes = Vec::new();
    file.take(length).read_to_end(&mut bytes).await.map_err(|error| io_error(target, &error))?;
    Ok(Some(bytes))
}

fn io_error(path: &str, error: &std::io::Error) -> FsError {
    match error.kind() {
        ErrorKind::NotFound | ErrorKind::NotADirectory => FsError::NotFound { path: path.to_owned() },
        _ => FsError::Failed { path: path.to_owned(), cause: error.to_string() },
    }
}
