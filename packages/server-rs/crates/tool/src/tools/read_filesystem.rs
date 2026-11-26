use futures_util::future::BoxFuture;
use opencode_util::path::Flavor;
use serde_json::{Value, json};

pub const MAX_READ_LINES: usize = 2_000;
pub const MAX_READ_BYTES: usize = 50 * 1024;
pub const MAX_MEDIA_INGEST_BYTES: u64 = 20 * 1024 * 1024;
const FIRST_CHUNK: u64 = 256 * 1024;
const TAIL_LINES: i64 = 100;
const MAX_LINE_LENGTH: usize = 2_000;
const LINE_BYTES: usize = 4 * (MAX_LINE_LENGTH + 1);
const MAX_LINE_SUFFIX: &str = "... (line truncated to 2000 chars)";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub kind: FileKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadRange {
    pub offset: u64,
    pub length: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadBytes {
    pub bytes: Vec<u8>,
    pub size: u64,
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReadFileError {
    #[error("Not found: {path}")]
    NotFound { path: String },
    #[error("{path} is a {actual:?}")]
    WrongKind { path: String, actual: FileKind },
    #[error("Failed at {path}: {cause}")]
    Failed { path: String, cause: String },
}

pub trait Files: Send + Sync {
    fn read<'a>(&'a self, path: &'a str, range: Option<ReadRange>) -> BoxFuture<'a, Result<ReadBytes, ReadFileError>>;
    fn list<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Vec<DirEntry>, ReadFileError>>;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LocalFiles;

impl Files for LocalFiles {
    fn read<'a>(&'a self, path: &'a str, range: Option<ReadRange>) -> BoxFuture<'a, Result<ReadBytes, ReadFileError>> {
        Box::pin(async move {
            if path.contains('\0') {
                return Err(ReadFileError::NotFound { path: path.to_owned() });
            }
            let metadata = tokio::fs::metadata(path).await.map_err(|error| io_error(path, &error))?;
            if metadata.is_dir() {
                return Err(ReadFileError::WrongKind { path: path.to_owned(), actual: FileKind::Directory });
            }
            if !metadata.is_file() {
                return Err(ReadFileError::WrongKind { path: path.to_owned(), actual: FileKind::Other });
            }
            let mut file = tokio::fs::File::open(path).await.map_err(|error| io_error(path, &error))?;
            let mut bytes = Vec::new();
            match range {
                Some(range) => {
                    use tokio::io::{AsyncReadExt, AsyncSeekExt};
                    if range.offset > 0 {
                        file.seek(std::io::SeekFrom::Start(range.offset))
                            .await
                            .map_err(|error| io_error(path, &error))?;
                    }
                    file.take(range.length).read_to_end(&mut bytes).await.map_err(|error| io_error(path, &error))?;
                }
                None => {
                    use tokio::io::AsyncReadExt;
                    file.read_to_end(&mut bytes).await.map_err(|error| io_error(path, &error))?;
                }
            }
            Ok(ReadBytes { bytes, size: metadata.len() })
        })
    }

    fn list<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Vec<DirEntry>, ReadFileError>> {
        Box::pin(async move {
            if path.contains('\0') {
                return Err(ReadFileError::NotFound { path: path.to_owned() });
            }
            let metadata = tokio::fs::metadata(path).await.map_err(|error| io_error(path, &error))?;
            if !metadata.is_dir() {
                return Err(ReadFileError::WrongKind { path: path.to_owned(), actual: FileKind::Other });
            }
            let mut reader = tokio::fs::read_dir(path).await.map_err(|error| io_error(path, &error))?;
            let mut entries = Vec::new();
            while let Some(entry) = reader.next_entry().await.map_err(|error| io_error(path, &error))? {
                let kind = match entry.file_type().await {
                    Ok(file_type) if file_type.is_dir() => FileKind::Directory,
                    Ok(file_type) if file_type.is_file() => FileKind::File,
                    Ok(file_type) if file_type.is_symlink() => FileKind::Symlink,
                    _ => FileKind::Other,
                };
                entries.push(DirEntry { name: entry.file_name().to_string_lossy().into_owned(), kind });
            }
            Ok(entries)
        })
    }
}

fn io_error(path: &str, error: &std::io::Error) -> ReadFileError {
    match error.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory => {
            ReadFileError::NotFound { path: path.to_owned() }
        }
        _ => ReadFileError::Failed { path: path.to_owned(), cause: error.to_string() },
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageInput {
    pub offset: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextContent {
    pub content: String,
    pub offset: u64,
    pub line_count: usize,
    pub next: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryContent {
    pub entries: Vec<String>,
    pub next: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaContent {
    pub uri: String,
    pub mime: String,
    pub name: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadOutput {
    Text(TextContent),
    Directory(DirectoryContent),
    Media(MediaContent),
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReadError {
    #[error("Cannot read binary file: {resource}")]
    Binary { resource: String },
    #[error("Media exceeds {maximum_bytes} byte ingestion limit: {resource}")]
    MediaIngestLimit { resource: String, maximum_bytes: u64 },
    #[error("Path is not {expected}: {resource}")]
    PathKind { resource: String, expected: &'static str },
    #[error("A negative offset only works for files, and {resource} is a directory")]
    DirectoryOffset { resource: String },
    #[error(transparent)]
    File(#[from] ReadFileError),
}

pub async fn read(files: &dyn Files, input: &str, resource: &str, page: &PageInput) -> Result<ReadOutput, ReadError> {
    let virtual_path = resource.starts_with("skill://");
    let first = match files.read(input, Some(ReadRange { offset: 0, length: FIRST_CHUNK })).await {
        Ok(first) => first,
        Err(ReadFileError::WrongKind { actual: FileKind::Directory, .. }) => {
            if page.offset.is_some_and(|offset| offset < 0) {
                return Err(ReadError::DirectoryOffset { resource: resource.to_owned() });
            }
            return match files.list(input).await {
                Ok(entries) => Ok(ReadOutput::Directory(list(entries, page, virtual_path))),
                Err(ReadFileError::WrongKind { .. }) => {
                    Err(ReadError::PathKind { resource: resource.to_owned(), expected: "a file or directory" })
                }
                Err(error) => Err(ReadError::File(error)),
            };
        }
        Err(ReadFileError::WrongKind { .. }) => {
            return Err(ReadError::PathKind { resource: resource.to_owned(), expected: "a file or directory" });
        }
        Err(error) => return Err(ReadError::File(error)),
    };

    if let Some(media) =
        opencode_fs::mime::detect_media(&first.bytes).filter(|media| opencode_fs::mime::MEDIA_MIMES.contains(media))
    {
        if first.size > MAX_MEDIA_INGEST_BYTES {
            return Err(ReadError::MediaIngestLimit {
                resource: resource.to_owned(),
                maximum_bytes: MAX_MEDIA_INGEST_BYTES,
            });
        }
        let whole = read_file(files, input, resource, None).await?;
        let uri = if virtual_path { resource.to_owned() } else { file_url(input) };
        let name = if virtual_path {
            input.rsplit('/').next().unwrap_or(input).to_owned()
        } else {
            Flavor::NATIVE.basename(input).to_owned()
        };
        return Ok(ReadOutput::Media(MediaContent { uri, mime: media.to_owned(), name, bytes: whole.bytes }));
    }

    let paged = first.size > MAX_READ_BYTES as u64 || page.offset.is_some() || page.limit.is_some();
    if !paged {
        if first.bytes.contains(&0) {
            return Err(ReadError::Binary { resource: resource.to_owned() });
        }
        let text = strip_bom(&String::from_utf8_lossy(&first.bytes)).to_owned();
        let lines: Vec<&str> = text.split('\n').collect();
        let count = if lines.last() == Some(&"") { lines.len() - 1 } else { lines.len() };
        if count <= MAX_READ_LINES {
            let content = lines.iter().map(|line| clamp_line(line)).collect::<Vec<_>>().join("\n");
            if content.len() <= MAX_READ_BYTES {
                return Ok(ReadOutput::Text(TextContent { content, offset: 1, line_count: count, next: None }));
            }
        }
    }

    text_page(files, input, resource, &first, page).await
}

async fn read_file(
    files: &dyn Files,
    input: &str,
    resource: &str,
    range: Option<ReadRange>,
) -> Result<ReadBytes, ReadError> {
    files.read(input, range).await.map_err(|error| match error {
        ReadFileError::WrongKind { .. } => ReadError::PathKind { resource: resource.to_owned(), expected: "a file" },
        error => ReadError::File(error),
    })
}

#[derive(Clone, Debug)]
struct Checkpoint {
    byte: usize,
    length: usize,
    first_line: i64,
    start: i64,
    last_line: i64,
}

struct TextPage<'a> {
    files: &'a dyn Files,
    input: &'a str,
    resource: &'a str,
    first_bytes: Vec<u8>,
    first_size: u64,
    requested: i64,
    limit: usize,
    tail_limit: i64,
    checkpoints: Vec<Checkpoint>,
    entries: Vec<String>,
    size: usize,
    from: i64,
    tail: bool,
    consumed: i64,
    next: Option<i64>,
    first_nul: Option<i64>,
}

async fn text_page(
    files: &dyn Files,
    input: &str,
    resource: &str,
    first: &ReadBytes,
    page: &PageInput,
) -> Result<ReadOutput, ReadError> {
    let requested = match page.offset {
        Some(0) | None => 1,
        Some(offset) => offset,
    };
    let limit = page.limit.filter(|limit| *limit != 0).unwrap_or(MAX_READ_LINES as i64);
    let limit = limit.clamp(0, MAX_READ_LINES as i64) as usize;
    let tail_limit = page.limit.filter(|limit| *limit != 0).unwrap_or(TAIL_LINES);
    let mut page = TextPage {
        files,
        input,
        resource,
        first_bytes: first.bytes.clone(),
        first_size: first.size,
        requested,
        limit,
        tail_limit,
        checkpoints: Vec::new(),
        entries: Vec::new(),
        size: 0,
        from: requested,
        tail: false,
        consumed: 0,
        next: None,
        first_nul: None,
    };
    page.run().await
}

impl TextPage<'_> {
    async fn run(&mut self) -> Result<ReadOutput, ReadError> {
        let mut byte = 0usize;
        let mut total = 0i64;
        let mut start = 0i64;
        let mut chunk = self.first_bytes.clone();
        let mut last_byte = 0usize;
        while !chunk.is_empty() {
            last_byte = byte;
            if self.first_nul.is_none() {
                self.first_nul = chunk.iter().position(|value| *value == 0).map(|nul| byte as i64 + nul as i64);
            }
            let count = chunk.iter().filter(|value| **value == 10).count() as i64;
            if count > 0 {
                let point =
                    Checkpoint { byte, length: chunk.len(), first_line: total + 1, start, last_line: total + count };
                self.checkpoints.push(point.clone());
                total += count;
                if self.requested > 0 && total >= self.requested {
                    self.collect(&point, &chunk).await?;
                }
                start = byte as i64 + chunk.iter().rposition(|value| *value == 10).unwrap() as i64 + 1;
            }
            byte += chunk.len();
            if self.next.is_none()
                && (self.entries.len() >= self.limit || self.size >= MAX_READ_BYTES)
                && self.consumed < byte as i64
            {
                self.next = Some(self.requested + self.entries.len() as i64);
            }
            if self.next.is_some() || byte as u64 >= self.first_size {
                break;
            }
            let more = self.range(byte as i64, FIRST_CHUNK as usize).await?;
            if more.is_empty() {
                break;
            }
            chunk = more;
        }

        if self.next.is_none() && start < byte as i64 {
            total += 1;
            let point = Checkpoint { byte: last_byte, length: chunk.len(), first_line: total, start, last_line: total };
            self.checkpoints.push(point.clone());
            if self.requested > 0 && total >= self.requested {
                self.collect(&point, &chunk).await?;
            }
        }
        if self.requested < 0 || self.requested > total {
            self.tail = self.requested > total;
            self.from = 1i64.max(total + 1 + if self.tail { -self.tail_limit.min(TAIL_LINES) } else { self.requested });
            let first_bytes = self.first_bytes.clone();
            let points = self.checkpoints.clone();
            for point in &points {
                if point.last_line < self.from {
                    continue;
                }
                let bytes = if point.byte == 0 {
                    first_bytes.clone()
                } else if point.byte == last_byte {
                    chunk.clone()
                } else {
                    self.reread(point.byte as i64, point.length).await?
                };
                self.collect(point, &bytes).await?;
                if self.next.is_some() {
                    break;
                }
            }
            if self.tail {
                while self.size > MAX_READ_BYTES {
                    let removed = self.entries.remove(0);
                    self.size = self.size.saturating_sub(removed.len() + 1);
                    self.from += 1;
                }
                self.consumed = byte as i64;
            }
        }

        if self.first_nul.is_some_and(|nul| nul < self.consumed) {
            return Err(ReadError::Binary { resource: self.resource.to_owned() });
        }
        Ok(ReadOutput::Text(TextContent {
            content: self.entries.join("\n"),
            offset: self.from.max(1) as u64,
            line_count: self.entries.len(),
            next: self.next.map(|next| next as u64),
        }))
    }

    async fn collect(&mut self, point: &Checkpoint, chunk: &[u8]) -> Result<(), ReadError> {
        let mut start = point.start;
        let mut local = (start - point.byte as i64).max(0);
        let mut line = point.first_line;
        while line < self.from && (local as usize) < chunk.len() {
            if chunk[local as usize] != 10 {
                local += 1;
                continue;
            }
            line += 1;
            start = point.byte as i64 + local + 1;
            local += 1;
        }
        line = line.max(point.first_line).max(self.from);
        while line <= point.last_line {
            if !self.tail && (self.entries.len() >= self.limit || self.size >= MAX_READ_BYTES) {
                self.next = Some(line);
                return Ok(());
            }
            let lf = if (local as usize) < chunk.len() {
                chunk[local as usize..].iter().position(|value| *value == 10).map(|index| local as usize + index)
            } else {
                None
            };
            let end = point.byte as i64 + lf.map_or(chunk.len() as i64, |index| index as i64);
            let length = (LINE_BYTES as i64).min((end - start).max(0)) as usize;
            let bytes: Vec<u8> = if start >= point.byte as i64 {
                let from = (start - point.byte as i64) as usize;
                chunk[from.min(chunk.len())..(from + length).min(chunk.len())].to_vec()
            } else if start + length as i64 <= self.first_bytes.len() as i64 {
                self.first_bytes[start as usize..(start as usize + length)].to_vec()
            } else {
                self.reread(start, length).await?
            };
            let mut text = String::from_utf8_lossy(&bytes).into_owned();
            if text.starts_with('\u{feff}')
                && (start == 0
                    || (!self.tail && line == self.from && self.first_bytes.len() < self.first_size as usize))
            {
                text.remove(0);
            }
            let text = clamp_line(text.strip_suffix('\r').unwrap_or(&text));
            let cost = text.len() + usize::from(!self.entries.is_empty());
            if !self.tail && self.size + cost > MAX_READ_BYTES {
                self.next = Some(line);
                return Ok(());
            }
            self.entries.push(text);
            self.size += cost;
            self.consumed = end + i64::from(lf.is_some());
            local = lf.map_or(-1, |index| index as i64) + 1;
            start = end + 1;
            line += 1;
        }
        Ok(())
    }

    async fn range(&self, offset: i64, length: usize) -> Result<Vec<u8>, ReadError> {
        let bytes = read_file(
            self.files,
            self.input,
            self.resource,
            Some(ReadRange { offset: offset.max(0) as u64, length: length as u64 }),
        )
        .await?;
        Ok(bytes.bytes)
    }

    async fn reread(&self, offset: i64, length: usize) -> Result<Vec<u8>, ReadError> {
        let mut bytes = self.range(offset, length).await?;
        if bytes.len() == length {
            return Ok(bytes);
        }
        let mut result = vec![0u8; length];
        result[..bytes.len()].copy_from_slice(&bytes);
        let mut filled = bytes.len();
        while filled < length {
            let more = self.range(offset + filled as i64, length - filled).await?;
            if more.is_empty() {
                break;
            }
            result[filled..filled + more.len()].copy_from_slice(&more);
            filled += more.len();
        }
        bytes = result;
        bytes.truncate(filled);
        Ok(bytes)
    }
}

fn list(items: Vec<DirEntry>, page: &PageInput, virtual_path: bool) -> DirectoryContent {
    let sep = if virtual_path { "/".to_owned() } else { Flavor::NATIVE.sep().to_string() };
    let offset = match page.offset {
        Some(0) | None => 1,
        Some(offset) => offset,
    };
    let limit = page.limit.filter(|limit| *limit != 0).unwrap_or(MAX_READ_LINES as i64);
    let limit = limit.clamp(0, MAX_READ_LINES as i64) as usize;
    let offset = offset.max(1) as usize;
    let mut visible: Vec<(String, bool)> = items
        .into_iter()
        .filter(|entry| entry.kind != FileKind::Other)
        .map(|entry| {
            let directory = entry.kind == FileKind::Directory;
            (if directory { format!("{}{sep}", entry.name) } else { entry.name }, directory)
        })
        .collect();
    visible.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    let selected: Vec<String> =
        visible.iter().skip(offset.saturating_sub(1)).take(limit).map(|entry| entry.0.clone()).collect();
    let next = if offset.saturating_sub(1) + selected.len() < visible.len() {
        Some((offset + selected.len()) as u64)
    } else {
        None
    };
    DirectoryContent { entries: selected, next }
}

fn clamp_line(line: &str) -> String {
    if line.encode_utf16().count() <= MAX_LINE_LENGTH {
        return line.to_owned();
    }
    format!("{}{MAX_LINE_SUFFIX}", head(line, MAX_LINE_LENGTH))
}

fn head(text: &str, limit: usize) -> &str {
    if text.encode_utf16().count() <= limit {
        return text;
    }
    let mut units = 0usize;
    for (index, character) in text.char_indices() {
        let next = units + character.len_utf16();
        if next > limit {
            return &text[..index];
        }
        units = next;
    }
    text
}

fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

pub fn file_url(path: &str) -> String {
    url::Url::from_file_path(path).map_or_else(|_| format!("file://{path}"), |url| url.to_string())
}

impl ReadOutput {
    pub fn to_value(&self) -> Value {
        match self {
            ReadOutput::Text(text) => {
                let mut value = json!({ "type": "text", "content": text.content, "offset": text.offset, "lineCount": text.line_count });
                if let Some(next) = text.next {
                    value["next"] = json!(next);
                }
                value
            }
            ReadOutput::Directory(directory) => {
                let mut value = json!({ "type": "directory", "entries": directory.entries });
                if let Some(next) = directory.next {
                    value["next"] = json!(next);
                }
                value
            }
            ReadOutput::Media(media) => {
                json!({ "type": "media", "uri": media.uri, "mime": media.mime, "name": media.name })
            }
        }
    }
}
