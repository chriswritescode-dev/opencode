use std::io::SeekFrom;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use http::{HeaderName, HeaderValue, StatusCode, header};
use opencode_contract::codec::encode_json;
use opencode_contract::generated::operations::{
    experimental_fs_unwatch, experimental_fs_watch, experimental_fs_write, fs_find, fs_list, fs_read,
};
use opencode_contract::generated::requests::FsFindQueryType;
use opencode_contract::generated::{
    AbsolutePath, ExperimentalFsWriteResponse, FileNotFoundError, FileNotFoundErrorTag, FileSystemWrite,
    FsFindResponse, FsListResponse, LocationRef, ServiceUnavailableError, ServiceUnavailableErrorTag, WorkspaceID,
};
use opencode_contract::request::RawRequest;
use opencode_fs::FsError;
use opencode_fs::file_watch::FileWatch;
use opencode_fs::filesystem::FileInfo;
use opencode_fs::search::{FindInput, FindKind};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::handlers::{defect, invalid, location_ref};
use crate::range::ByteRange;
use crate::response;
use crate::services::LocationServices;

const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; img-src 'self'; media-src 'self'; sandbox";
const TEXT_TYPE: &str = "text/plain; charset=utf-8";
const CHUNK: usize = 64 * 1024;

pub async fn read(method: &http::Method, raw: &RawRequest, services: &LocationServices) -> response::Response {
    if let Err(error) = fs_read::decode_query(raw) {
        return invalid::request(invalid::Kind::Query, error);
    }
    let path = wildcard(raw);
    let info = match services.files.filesystem.read(&path).await {
        Ok(info) => info,
        Err(FsError::NotFound { path }) => return file_not_found(&path),
        Err(error) => return defect::response("failed to read the file", error),
    };
    serve_file(method, raw, &path, &info).await
}

pub async fn list(raw: &RawRequest, services: &LocationServices) -> response::Response {
    let query = match fs_list::decode_query(raw) {
        Ok(query) => query,
        Err(error) => return invalid::request(invalid::Kind::Query, error),
    };
    match services.files.filesystem.list(query.path.flatten().as_deref()).await {
        Ok(entries) => response::bytes(
            StatusCode::OK,
            "application/json",
            encode_json(&FsListResponse {
                location: location_ref(services),
                data: entries.iter().map(|entry| entry.to_contract()).collect(),
            }),
        ),
        Err(FsError::NotFound { path }) => directory_not_found(&path),
        Err(error) => defect::response("failed to list the directory", error),
    }
}

pub async fn find(raw: &RawRequest, services: &LocationServices) -> response::Response {
    let query = match fs_find::decode_query(raw) {
        Ok(query) => query,
        Err(error) => return invalid::request(invalid::Kind::Query, error),
    };
    let input = FindInput {
        query: query.query,
        kind: match query.r#type.flatten() {
            Some(FsFindQueryType::File) => FindKind::File,
            Some(FsFindQueryType::Directory) => FindKind::Directory,
            None => FindKind::All,
        },
        limit: query.limit.flatten().map_or(opencode_fs::search::DEFAULT_LIMIT, |limit| limit as usize),
    };
    match services.files.search.find(input).await {
        Ok(entries) => response::bytes(
            StatusCode::OK,
            "application/json",
            encode_json(&FsFindResponse {
                location: location_ref(services),
                data: entries.iter().map(|entry| entry.to_contract()).collect(),
            }),
        ),
        Err(error) => defect::response("failed to find files", error),
    }
}

pub async fn write(raw: &RawRequest, services: &LocationServices) -> response::Response {
    let request = match experimental_fs_write::decode(raw) {
        Ok(request) => request,
        Err(error) => return invalid::request(invalid::Kind::Query, error),
    };
    match services.files.filesystem.write(&request.query.path, &request.body).await {
        Ok(target) => response::bytes(
            StatusCode::OK,
            "application/json",
            encode_json(&ExperimentalFsWriteResponse {
                location: location_ref(services),
                data: FileSystemWrite { path: AbsolutePath(target.to_string_lossy().into_owned()) },
            }),
        ),
        Err(error) => defect::response("failed to write the file", error),
    }
}

pub async fn watch(raw: &RawRequest, services: &LocationServices, file_watch: &FileWatch) -> response::Response {
    if let Err(error) = experimental_fs_watch::decode_query(raw) {
        return invalid::request(invalid::Kind::Query, error);
    }
    let body = match experimental_fs_watch::decode_body(raw) {
        Ok(body) => body,
        Err(error) => return invalid::request(invalid::Kind::Body, error),
    };
    if let Err(error) = services.files.filesystem.read(&body.path).await {
        return match error {
            FsError::NotFound { path } => file_not_found(&path),
            error => defect::response("failed to read the watched file", error),
        };
    }
    let input = opencode_fs::file_watch::Input {
        location: LocationRef {
            directory: AbsolutePath(services.info.directory.clone()),
            workspace_id: services.info.workspace_id.clone().map(WorkspaceID),
        },
        file: services.files.filesystem.absolute(&body.path),
    };
    match file_watch.watch(input).await {
        Ok(()) => response::empty(StatusCode::NO_CONTENT),
        Err(error) => response::bytes(
            StatusCode::SERVICE_UNAVAILABLE,
            "application/json",
            encode_json(&ServiceUnavailableError {
                tag: ServiceUnavailableErrorTag::ServiceUnavailableError,
                message: error.to_string(),
                service: Some(Some("filewatcher".to_owned())),
            }),
        ),
    }
}

pub async fn unwatch(raw: &RawRequest, services: &LocationServices, file_watch: &FileWatch) -> response::Response {
    if let Err(error) = experimental_fs_unwatch::decode_query(raw) {
        return invalid::request(invalid::Kind::Query, error);
    }
    let body = match experimental_fs_unwatch::decode_body(raw) {
        Ok(body) => body,
        Err(error) => return invalid::request(invalid::Kind::Body, error),
    };
    file_watch
        .unwatch(opencode_fs::file_watch::Input {
            location: LocationRef {
                directory: AbsolutePath(services.info.directory.clone()),
                workspace_id: services.info.workspace_id.clone().map(WorkspaceID),
            },
            file: services.files.filesystem.absolute(&body.path),
        })
        .await;
    response::empty(StatusCode::NO_CONTENT)
}

async fn serve_file(method: &http::Method, raw: &RawRequest, path: &str, info: &FileInfo) -> response::Response {
    let etag = etag(info);
    let last_modified = info.mtime.map(httpdate::fmt_http_date);
    let content = content_headers(path, &info.mime);
    let mut headers = validators(&etag, last_modified.as_deref());
    headers.extend(clone_headers(&content));

    if is_not_modified(raw, &etag, last_modified.as_deref()) {
        return file_response(StatusCode::NOT_MODIFIED, headers, response::Body::empty());
    }

    let range_header = if method == http::Method::GET { header(raw, "range") } else { None };
    let range = range_header
        .filter(|_| matches_if_range(header(raw, "if-range"), &etag, last_modified.as_deref()))
        .and_then(|value| crate::range::parse(Some(value), info.size));
    if range == Some(ByteRange::Unsatisfiable) {
        return file_response(
            StatusCode::RANGE_NOT_SATISFIABLE,
            vec![
                (header::ACCEPT_RANGES, "bytes".to_owned()),
                (header::CONTENT_RANGE, format!("bytes */{}", info.size)),
            ],
            response::Body::empty(),
        );
    }

    let (offset, length, status) = match range {
        Some(ByteRange::Satisfiable { start, end }) => (start, end - start + 1, StatusCode::PARTIAL_CONTENT),
        _ => (0, info.size, StatusCode::OK),
    };
    headers.extend(content);
    headers.push((header::ACCEPT_RANGES, "bytes".to_owned()));
    if let Some(ByteRange::Satisfiable { start, end }) = range {
        headers.push((header::CONTENT_RANGE, format!("bytes {start}-{end}/{}", info.size)));
    }
    headers.push((header::CONTENT_LENGTH, length.to_string()));

    if method == http::Method::HEAD || length == 0 {
        return file_response(status, headers, response::Body::empty());
    }
    let mut file = match tokio::fs::File::open(&info.path).await {
        Ok(file) => file,
        Err(error) => return defect::response("failed to open the file", error),
    };
    if offset > 0
        && let Err(error) = file.seek(SeekFrom::Start(offset)).await
    {
        return defect::response("failed to seek the file", error);
    }
    file_response(status, headers, response::Body::Stream(file_stream(file, length)))
}

fn file_response(status: StatusCode, headers: Vec<(HeaderName, String)>, body: response::Body) -> response::Response {
    let mut response = response::Response::new(body);
    *response.status_mut() = status;
    for (name, value) in headers {
        if let Ok(value) = HeaderValue::from_str(&value) {
            response.headers_mut().insert(name, value);
        }
    }
    response
}

fn file_stream(file: tokio::fs::File, length: u64) -> response::ByteStream {
    Box::pin(futures_util::stream::try_unfold((file, length), |(mut file, remaining)| async move {
        if remaining == 0 {
            return Ok(None);
        }
        let mut buffer = vec![0u8; remaining.min(CHUNK as u64) as usize];
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            return Ok(None);
        }
        buffer.truncate(read);
        Ok(Some((Bytes::from(buffer), (file, remaining - read as u64))))
    }))
}

fn etag(info: &FileInfo) -> String {
    let modified = info.mtime.map(millis).map_or_else(|| "0".to_owned(), |millis| format!("{millis:x}"));
    format!("W/\"{:x}-{modified}\"", info.size)
}

fn millis(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis() as i64,
        Err(error) => -(error.duration().as_millis() as i64),
    }
}

fn validators(etag: &str, last_modified: Option<&str>) -> Vec<(HeaderName, String)> {
    let mut headers =
        vec![(header::CACHE_CONTROL, "no-cache, no-transform".to_owned()), (header::ETAG, etag.to_owned())];
    if let Some(last_modified) = last_modified {
        headers.push((header::LAST_MODIFIED, last_modified.to_owned()));
    }
    headers
}

fn content_headers(path: &str, mime: &str) -> Vec<(HeaderName, String)> {
    vec![
        (header::CONTENT_TYPE, content_type(mime).to_owned()),
        (header::CONTENT_DISPOSITION, disposition(&basename(path))),
        (HeaderName::from_static("content-security-policy"), CONTENT_SECURITY_POLICY.to_owned()),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
    ]
}

fn clone_headers(headers: &[(HeaderName, String)]) -> Vec<(HeaderName, String)> {
    headers.iter().map(|(name, value)| (name.clone(), value.clone())).collect()
}

fn content_type(mime: &str) -> &str {
    let lower = mime.to_ascii_lowercase();
    let textual = ["text/html", "text/javascript", "application/javascript", "application/x-javascript"]
        .iter()
        .any(|prefix| lower.starts_with(prefix) && word_boundary_after(&lower, prefix.len()))
        || ["/xml", "+xml"].iter().any(|needle| {
            lower.match_indices(needle).any(|(index, _)| word_boundary_after(&lower, index + needle.len()))
        });
    if textual { TEXT_TYPE } else { mime }
}

fn word_boundary_after(text: &str, index: usize) -> bool {
    text.as_bytes().get(index).is_none_or(|byte| !(byte.is_ascii_alphanumeric() || *byte == b'_'))
}

fn basename(path: &str) -> String {
    Path::new(path).file_name().map_or_else(|| path.to_owned(), |name| name.to_string_lossy().into_owned())
}

fn disposition(name: &str) -> String {
    if name.is_empty() {
        return "attachment".to_owned();
    }
    let fallback: String = name
        .chars()
        .flat_map(|character| {
            let code = character as u32;
            let keep = (0x20..=0x7e).contains(&code) && !matches!(character, '"' | '\\' | '%');
            let count = if keep { 1 } else { character.len_utf16() };
            (0..count).map(move |_| if keep { character } else { '_' })
        })
        .collect();
    let encoded = opencode_api_types::js::encode_component(name)
        .chars()
        .flat_map(|character| match character {
            '\'' => "%27".chars().collect::<Vec<_>>(),
            '(' => "%28".chars().collect(),
            ')' => "%29".chars().collect(),
            '*' => "%2A".chars().collect(),
            other => vec![other],
        })
        .collect::<String>();
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

fn is_not_modified(raw: &RawRequest, etag: &str, last_modified: Option<&str>) -> bool {
    if let Some(if_none_match) = header(raw, "if-none-match") {
        return if_none_match.split(',').any(|tag| tag.trim() == "*" || strip_weak(tag) == strip_weak(etag));
    }
    let (Some(if_modified_since), Some(last_modified)) = (header(raw, "if-modified-since"), last_modified) else {
        return false;
    };
    match (httpdate::parse_http_date(if_modified_since.trim()), httpdate::parse_http_date(last_modified)) {
        (Ok(since), Ok(modified)) => modified <= since,
        _ => false,
    }
}

fn matches_if_range(if_range: Option<&str>, etag: &str, last_modified: Option<&str>) -> bool {
    let Some(if_range) = if_range else {
        return true;
    };
    let trimmed = if_range.trim_start();
    let quoted = trimmed.starts_with('"')
        || (trimmed.get(..2).is_some_and(|prefix| prefix.eq_ignore_ascii_case("w/"))
            && trimmed.get(2..).is_some_and(|rest| rest.starts_with('"')));
    if quoted {
        return strip_weak(if_range) == strip_weak(etag);
    }
    let Some(last_modified) = last_modified else {
        return false;
    };
    match (httpdate::parse_http_date(if_range.trim()), httpdate::parse_http_date(last_modified)) {
        (Ok(requested), Ok(modified)) => requested == modified,
        _ => false,
    }
}

fn strip_weak(etag: &str) -> &str {
    let trimmed = etag.trim();
    if trimmed.get(..2).is_some_and(|prefix| prefix.eq_ignore_ascii_case("w/")) { &trimmed[2..] } else { trimmed }
}

fn header<'a>(raw: &'a RawRequest, name: &str) -> Option<&'a str> {
    raw.headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
}

fn wildcard(raw: &RawRequest) -> String {
    raw.path_params.iter().find(|(name, _)| name == "*").map(|(_, value)| value.clone()).unwrap_or_default()
}

fn file_not_found(path: &str) -> response::Response {
    response::bytes(
        StatusCode::NOT_FOUND,
        "application/json",
        encode_json(&FileNotFoundError {
            tag: FileNotFoundErrorTag::FileNotFoundError,
            path: path.to_owned(),
            message: format!("File not found: {path}"),
        }),
    )
}

fn directory_not_found(path: &str) -> response::Response {
    response::bytes(
        StatusCode::NOT_FOUND,
        "application/json",
        encode_json(&FileNotFoundError {
            tag: FileNotFoundErrorTag::FileNotFoundError,
            path: path.to_owned(),
            message: format!("Directory not found: {path}"),
        }),
    )
}
