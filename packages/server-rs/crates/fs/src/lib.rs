pub mod access;
pub mod bom;
pub mod diff;
pub mod file_watch;
pub mod filesystem;
pub mod formatter;
pub mod generated;
pub mod mime;
pub mod mutation;
pub mod mutex;
pub mod path;
pub mod protected;
pub mod retention;
pub mod ripgrep;
pub mod search;
pub mod watcher;
pub mod write;

#[derive(Clone, Debug, thiserror::Error)]
pub enum FsError {
    #[error("Not found: {path}")]
    NotFound { path: String },
    #[error("Directory not found: {directory}")]
    DirectoryNotFound { directory: String },
    #[error("Access denied to directory: {directory}")]
    DirectoryAccessDenied { directory: String },
    #[error("Failed at {path}: {cause}")]
    Failed { path: String, cause: String },
    #[error(
        "file is not valid UTF-8. It was left unchanged because rewriting it as text would replace every byte that is not UTF-8 with U+FFFD; change it with a tool that preserves its bytes, or convert it to UTF-8 first"
    )]
    InvalidUtf8 { path: String },
}
