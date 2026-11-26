use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value};
use unicode_segmentation::UnicodeSegmentation;

use crate::context::{Content, ToolOutput};

pub const MAX_LINES: usize = 2_000;
pub const MAX_BYTES: usize = 50 * 1024;
pub const RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);
pub const DIRECTORY: &str = "tool-output";

pub fn file_name(timestamp: i64) -> String {
    format!("tool_{}", opencode_storage::id::create(false, timestamp))
}

pub fn directory(data: &Path) -> PathBuf {
    data.join(DIRECTORY)
}

#[derive(Clone)]
pub struct OutputStore {
    directory: PathBuf,
    max_lines: usize,
    max_bytes: usize,
}

impl OutputStore {
    pub fn new(data: impl Into<PathBuf>) -> Self {
        OutputStore { directory: directory(&data.into()), max_lines: MAX_LINES, max_bytes: MAX_BYTES }
    }

    pub fn configure(&mut self, max_lines: Option<usize>, max_bytes: Option<usize>) {
        if let Some(max_lines) = max_lines {
            self.max_lines = max_lines;
        }
        if let Some(max_bytes) = max_bytes {
            self.max_bytes = max_bytes;
        }
    }

    pub fn observe(&mut self, entries: &[opencode_config::entry::Entry]) {
        let Some(configured) = opencode_config::entry::latest(entries, |info| info.tool_output.clone()) else {
            return;
        };
        self.configure(
            configured.max_lines.map(|value| value as usize),
            configured.max_bytes.map(|value| value as usize),
        );
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub async fn truncate(&self, mut result: ToolOutput) -> ToolOutput {
        if result.metadata.as_ref().and_then(|metadata| metadata.get("truncated")).is_some() {
            return result;
        }
        let text = result
            .content
            .iter()
            .filter_map(|item| match item {
                Content::Text { text } => Some(text.as_str()),
                Content::File { .. } => None,
            })
            .fold(String::new(), |mut joined, text| {
                if !joined.is_empty() && !joined.ends_with('\n') {
                    joined.push('\n');
                }
                joined.push_str(text);
                joined
            });
        let total_lines = count_lines(&text);
        if total_lines <= self.max_lines && text.len() <= self.max_bytes {
            result.metadata = Some(metadata(result.metadata, false, None));
            return result;
        }

        let end = after_lines(&text, self.max_lines).min(byte_boundary(&text, self.max_bytes));
        let shown_end = if text[..end].ends_with("\r\n") {
            end - 2
        } else if text[..end].ends_with('\n') {
            end - 1
        } else {
            end
        };
        let file = self.directory.join(file_name(opencode_storage::id::now_ms()));
        let failure = self.save(&file, &text).await.err();
        let shown_lines = count_lines(&text[..end]);
        let last_line_cut = shown_lines > 0
            && shown_end < text.len()
            && !text[shown_end..].starts_with('\n')
            && !text[shown_end..].starts_with("\r\n");
        let range = if shown_lines == 0 { "0 lines".to_owned() } else { format!("lines 1-{shown_lines}") };
        let saved = match &failure {
            None => format!("full output saved to {}", file.to_string_lossy()),
            Some(error) => format!("full output could not be saved: {error}"),
        };
        let marker = format!(
            "[showing {range} of {total_lines}{}; {saved}]",
            if last_line_cut { " (last line cut)" } else { "" }
        );
        let output_path = failure.is_none().then_some(file.as_path());
        let bounded = bound_content(std::mem::take(&mut result.content), shown_end, &marker);
        result.content = bounded;
        result.metadata = Some(metadata(result.metadata, true, output_path));
        result
    }

    async fn save(&self, file: &Path, text: &str) -> std::io::Result<()> {
        tokio::fs::create_dir_all(&self.directory).await?;
        tokio::fs::write(file, text.as_bytes()).await
    }

    pub async fn cleanup(&self) {
        cleanup(self.directory.clone()).await;
    }
}

fn metadata(metadata: Option<Value>, truncated: bool, output_path: Option<&Path>) -> Value {
    let mut map = match metadata {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    };
    map.insert("truncated".to_owned(), Value::Bool(truncated));
    if let Some(path) = output_path {
        map.insert("outputPath".to_owned(), Value::String(path.to_string_lossy().into_owned()));
    }
    Value::Object(map)
}

fn bound_content(content: Vec<Content>, shown: usize, marker: &str) -> Vec<Content> {
    let mut bounded = Vec::new();
    let mut remaining = shown;
    let mut separator = false;
    let mut marked = false;
    for item in content {
        let Content::Text { text } = item else {
            bounded.push(item);
            continue;
        };
        if separator && remaining > 0 {
            remaining -= 1;
        }
        separator = !text.is_empty() && !text.ends_with('\n');
        if !marked && remaining >= text.len() {
            remaining -= text.len();
            bounded.push(Content::Text { text });
            continue;
        }
        if remaining > 0 {
            bounded.push(Content::Text { text: text[..remaining].to_owned() });
        }
        if !marked {
            bounded.push(Content::text(marker));
        }
        remaining = 0;
        marked = true;
    }
    if !marked {
        bounded.push(Content::text(marker));
    }
    bounded
}

fn count_lines(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let bytes = text.as_bytes();
    1 + bytes[..bytes.len() - 1].iter().filter(|byte| **byte == b'\n').count()
}

fn after_lines(text: &str, count: usize) -> usize {
    let bytes = text.as_bytes();
    let mut index = 0;
    for _ in 0..count {
        match bytes[index..].iter().position(|byte| *byte == b'\n') {
            Some(offset) => index += offset + 1,
            None => return text.len(),
        }
    }
    index
}

fn byte_boundary(text: &str, budget: usize) -> usize {
    let mut used = 0;
    let mut cut = 0;
    for (index, character) in text.char_indices() {
        let size = character.len_utf8();
        if used + size > budget {
            cut = index;
            break;
        }
        used += size;
        cut = index + size;
    }
    if cut == 0 || cut == text.len() {
        return cut;
    }
    let previous = text[..cut].chars().next_back().expect("a non-empty prefix has a character");
    let next = text[cut..].chars().next().expect("a prefix shorter than the text has a following character");
    if (previous as u32) < 0x80 && (next as u32) < 0x80 {
        return if previous == '\r' && next == '\n' { cut - 1 } else { cut };
    }
    let start = text[..cut].rfind('\n').map_or(0, |index| index + 1);
    let mut boundary = start;
    for (offset, _) in text[start..].grapheme_indices(true) {
        if start + offset > cut {
            break;
        }
        boundary = start + offset;
    }
    boundary
}

pub async fn cleanup(directory: PathBuf) {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&directory) {
        for entry in entries.flatten() {
            if is_output_name(&entry.file_name().to_string_lossy()) {
                files.push(entry.path().to_string_lossy().into_owned());
            }
        }
    }
    opencode_fs::retention::cleanup(&files, RETENTION).await;
}

fn is_output_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("tool_") else { return false };
    rest.len() >= 12 && rest.as_bytes()[..12].iter().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}
