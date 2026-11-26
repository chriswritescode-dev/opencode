use std::io::ErrorKind;
use std::path::PathBuf;
use std::time::SystemTime;

use opencode_contract::generated::{FileSystemEntry, FileSystemEntryType, RelativePath};
use opencode_util::path::Flavor;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::{FsError, mime, path};

#[derive(Clone, Debug, PartialEq)]
pub struct FileInfo {
    pub mime: String,
    pub size: u64,
    pub mtime: Option<SystemTime>,
    pub path: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub kind: EntryKind,
}

impl Entry {
    pub fn to_contract(&self) -> FileSystemEntry {
        FileSystemEntry {
            path: RelativePath(self.path.clone()),
            r#type: match self.kind {
                EntryKind::File => FileSystemEntryType::File,
                EntryKind::Directory => FileSystemEntryType::Directory,
            },
        }
    }
}

pub struct LocationFileSystem {
    directory: String,
    root: String,
}

impl LocationFileSystem {
    pub async fn new(directory: impl Into<String>) -> Result<Self, FsError> {
        let directory = directory.into();
        let real = tokio::fs::canonicalize(&directory).await.map_err(|error| directory_error(&directory, &error))?;
        let root = match real.to_str() {
            Some(text) => opencode_util::path::strip_verbatim(text).into_owned(),
            None => return Err(FsError::Failed { path: directory, cause: "directory is not valid UTF-8".to_owned() }),
        };
        let _readable = tokio::fs::read_dir(&root).await.map_err(|error| directory_error(&directory, &error))?;
        Ok(Self { directory, root })
    }

    pub fn directory(&self) -> &str {
        &self.directory
    }

    pub fn absolute(&self, input: &str) -> String {
        Flavor::NATIVE.resolve(&[&self.directory, input])
    }

    pub async fn read(&self, input: &str) -> Result<FileInfo, FsError> {
        let (_absolute, real) = self.resolve(input).await?;
        let metadata = tokio::fs::metadata(&real).await.map_err(|error| io_error(&real, &error))?;
        if !metadata.is_file() {
            return Err(FsError::NotFound { path: input.to_owned() });
        }
        Ok(FileInfo {
            mime: mime::lookup(&real).to_owned(),
            size: metadata.len(),
            mtime: metadata.modified().ok(),
            path: real,
        })
    }

    pub async fn open_range(&self, info: &FileInfo, offset: u64, length: Option<u64>) -> Result<Vec<u8>, FsError> {
        let mut file = tokio::fs::File::open(&info.path).await.map_err(|error| io_error(&info.path, &error))?;
        if offset > 0 {
            file.seek(std::io::SeekFrom::Start(offset)).await.map_err(|error| io_error(&info.path, &error))?;
        }
        let mut bytes = Vec::new();
        match length {
            Some(length) => {
                file.take(length).read_to_end(&mut bytes).await.map_err(|error| io_error(&info.path, &error))?;
            }
            None => {
                file.read_to_end(&mut bytes).await.map_err(|error| io_error(&info.path, &error))?;
            }
        }
        Ok(bytes)
    }

    pub async fn list(&self, input: Option<&str>) -> Result<Vec<Entry>, FsError> {
        let flavor = Flavor::NATIVE;
        let requested = input.unwrap_or(".");
        let directory = flavor.resolve(&[&self.directory, requested]);
        let missing = FsError::NotFound { path: requested.to_owned() };
        let metadata = tokio::fs::metadata(&directory).await.map_err(|error| match error.kind() {
            ErrorKind::NotFound | ErrorKind::NotADirectory => missing.clone(),
            _ => FsError::Failed { path: directory.clone(), cause: error.to_string() },
        })?;
        if !metadata.is_dir() {
            return Err(missing);
        }
        let mut reader = tokio::fs::read_dir(&directory)
            .await
            .map_err(|error| FsError::Failed { path: directory.clone(), cause: error.to_string() })?;
        let mut entries = Vec::new();
        while let Some(item) = reader
            .next_entry()
            .await
            .map_err(|error| FsError::Failed { path: directory.clone(), cause: error.to_string() })?
        {
            let kind = match item.file_type().await {
                Ok(file_type) if file_type.is_dir() => EntryKind::Directory,
                Ok(file_type) if file_type.is_file() => EntryKind::File,
                _ => continue,
            };
            let absolute = flavor.join(&[&directory, &item.file_name().to_string_lossy()]);
            let relative = flavor.relative(&self.directory, &absolute);
            let relative = if relative.is_empty() { ".".to_owned() } else { relative };
            let path = match kind {
                EntryKind::Directory => format!("{relative}{}", flavor.sep()),
                EntryKind::File => relative,
            };
            entries.push(Entry { path, kind });
        }
        entries.sort_by(|left, right| match (left.kind, right.kind) {
            (EntryKind::Directory, EntryKind::File) => std::cmp::Ordering::Less,
            (EntryKind::File, EntryKind::Directory) => std::cmp::Ordering::Greater,
            _ => left.path.cmp(&right.path),
        });
        Ok(entries)
    }

    pub async fn write(&self, input: &str, bytes: &[u8]) -> Result<PathBuf, FsError> {
        let target = Flavor::NATIVE.resolve(&[&self.directory, input]);
        write_with_dirs(&target, bytes).await?;
        Ok(PathBuf::from(target))
    }

    pub async fn find(
        &self,
        search: &crate::search::Search,
        input: crate::search::FindInput,
    ) -> Result<Vec<Entry>, FsError> {
        search
            .find(input)
            .await
            .map_err(|error| FsError::Failed { path: self.directory.clone(), cause: error.to_string() })
    }

    async fn resolve(&self, input: &str) -> Result<(String, String), FsError> {
        if input.contains('\u{0}') {
            return Err(FsError::NotFound { path: input.to_owned() });
        }
        let absolute = Flavor::NATIVE.resolve(&[&self.directory, input]);
        if !path::contains(&self.directory, &absolute) {
            return Err(FsError::NotFound { path: input.to_owned() });
        }
        let real = tokio::fs::canonicalize(&absolute).await.map_err(|error| match error.kind() {
            ErrorKind::NotFound | ErrorKind::NotADirectory => FsError::NotFound { path: input.to_owned() },
            _ => FsError::Failed { path: absolute.clone(), cause: error.to_string() },
        })?;
        let real = match real.to_str() {
            Some(text) => opencode_util::path::strip_verbatim(text).into_owned(),
            None => return Err(FsError::NotFound { path: input.to_owned() }),
        };
        if !path::contains(&self.root, &real) {
            return Err(FsError::NotFound { path: input.to_owned() });
        }
        Ok((absolute, real))
    }
}

pub async fn write_with_dirs(target: &str, bytes: &[u8]) -> Result<(), FsError> {
    match tokio::fs::write(target, bytes).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let parent = path::dirname(target);
            tokio::fs::create_dir_all(&parent)
                .await
                .map_err(|error| FsError::Failed { path: parent.clone(), cause: error.to_string() })?;
            tokio::fs::write(target, bytes)
                .await
                .map_err(|error| FsError::Failed { path: target.to_owned(), cause: error.to_string() })
        }
        Err(error) => Err(FsError::Failed { path: target.to_owned(), cause: error.to_string() }),
    }
}

fn directory_error(directory: &str, error: &std::io::Error) -> FsError {
    match error.kind() {
        ErrorKind::NotFound | ErrorKind::NotADirectory => {
            FsError::DirectoryNotFound { directory: directory.to_owned() }
        }
        ErrorKind::PermissionDenied => FsError::DirectoryAccessDenied { directory: directory.to_owned() },
        _ => FsError::Failed { path: directory.to_owned(), cause: error.to_string() },
    }
}

fn io_error(path: &str, error: &std::io::Error) -> FsError {
    match error.kind() {
        ErrorKind::NotFound | ErrorKind::NotADirectory => FsError::NotFound { path: path.to_owned() },
        _ => FsError::Failed { path: path.to_owned(), cause: error.to_string() },
    }
}
