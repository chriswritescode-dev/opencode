use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use opencode_contract::generated::ShellOutputResponseData;

use crate::result::Capture;

pub const DEFAULT_MAX_LINES: usize = 2_000;
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024;
pub const COUNT_NEWLINES_LIMIT: u64 = 16 * 1024 * 1024;
pub const LINE_START_SCAN_LIMIT: u64 = 1024 * 1024;
const DEFAULT_PAGE_LIMIT: u64 = 65_536;

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_lines: usize,
    pub max_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_lines: DEFAULT_MAX_LINES, max_bytes: DEFAULT_MAX_BYTES }
    }
}

pub fn page(file: &Path, size: u64, cursor: Option<u64>, limit: Option<u64>, running: bool) -> ShellOutputResponseData {
    let cursor = cursor.unwrap_or(0);
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT);
    if cursor >= size {
        return ShellOutputResponseData {
            output: String::new(),
            cursor: size as f64,
            size: size as f64,
            truncated: false,
        };
    }
    let start = cursor;
    if limit == 0 {
        return ShellOutputResponseData {
            output: String::new(),
            cursor: start as f64,
            size: size as f64,
            truncated: false,
        };
    }
    let offset = start.saturating_sub(3);
    let length = size.min(start.saturating_add(limit).saturating_add(3)).saturating_sub(offset);
    let bytes = read_range(file, offset, length);
    let from = usize::try_from(start - offset).unwrap_or(0);
    let to = bytes.len().min(from.saturating_add(usize::try_from(limit).unwrap_or(usize::MAX)));
    let first = split_character(&bytes, from, running).and_then(|split| split.end).unwrap_or(from);
    let last = split_character(&bytes, to, running);
    let end = match last {
        Some(split) if split.start == first && split.end.is_some() => split.end.unwrap_or(to),
        Some(split) => split.start,
        None => to,
    };
    let output = if first < end && end <= bytes.len() {
        String::from_utf8_lossy(&bytes[first..end]).into_owned()
    } else {
        String::new()
    };
    ShellOutputResponseData {
        output,
        cursor: (offset + first.max(end).min(bytes.len()) as u64) as f64,
        size: size as f64,
        truncated: false,
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Newlines {
    Known(u64),
    Scan,
}

#[derive(Clone, Copy, Debug)]
pub enum LineStarts {
    Known([u64; 2]),
    Scan,
}

#[derive(Clone, Debug)]
pub struct Tail {
    pub size: u64,
    pub newlines: Newlines,
    pub line_starts: LineStarts,
    pub stopped_early: Option<String>,
}

pub fn tail(file: &Path, tail: Tail, limits: Limits, killed: Option<String>) -> Capture {
    let size = tail.size;
    let max_bytes = limits.max_bytes as u64;
    let start = size.saturating_sub(max_bytes);
    let before = start.saturating_sub(1);
    let window = read_range(file, before, size.saturating_sub(before));
    let page = utf8_tail(&window, size.saturating_sub(start));
    let partial = start > 0 && window.first().is_some_and(|byte| *byte != b'\n');
    let ends_with_newline = page.ends_with('\n');
    let mut lines: Vec<&str> = page.split('\n').collect();
    if ends_with_newline {
        lines.pop();
    }
    let newlines = match tail.newlines {
        Newlines::Known(newlines) => Some(newlines),
        Newlines::Scan => count_newlines(file, size),
    };
    let total = newlines.map(|newlines| newlines + u64::from(!ends_with_newline));
    let stopped =
        tail.stopped_early.map(|message| format!("\n\n[output file {} stopped early: {message}]", file.display()));
    let suffix = match &stopped {
        Some(stopped) => format!("full output saved to {}]{stopped}", file.display()),
        None => format!("full output saved to {}]", file.display()),
    };
    if partial && lines.len() == 1 {
        let line_start = match tail.line_starts {
            LineStarts::Known(starts) => Some(if ends_with_newline { starts[0] } else { starts[1] }),
            LineStarts::Scan => scan_line_starts(file, size)[usize::from(!ends_with_newline)],
        };
        let end = size.saturating_sub(u64::from(ends_with_newline));
        let line_size = match line_start {
            Some(line_start) => format_size(end.saturating_sub(line_start)),
            None => format!("over {}", format_size(end.min(LINE_START_SCAN_LIMIT))),
        };
        let shown_size = format_size(lines[0].len() as u64);
        let line = total.map_or_else(|| "the last line".to_owned(), |total| format!("line {total}"));
        return Capture {
            output: format!("{}\n\n[showing the last {shown_size} of {line} ({line_size}); {suffix}", lines[0]),
            truncated: true,
            killed,
        };
    }
    if start > 0 || lines.len() > limits.max_lines {
        let available = lines.len().saturating_sub(usize::from(partial));
        let kept = &lines[lines.len().saturating_sub(limits.max_lines.min(available))..];
        let notice = match total {
            Some(total) => format!("showing lines {}-{total} of {total}", total - kept.len() as u64 + 1),
            None => format!("showing the last {} lines", kept.len()),
        };
        return Capture { output: format!("{}\n\n[{notice}; {suffix}", kept.join("\n")), truncated: true, killed };
    }
    let shown = match &stopped {
        Some(_) => page.strip_suffix('\n').unwrap_or(&page),
        None => &page,
    };
    let cleaned = if shown.is_empty() { "(no output)" } else { shown };
    Capture { output: format!("{cleaned}{}", stopped.as_deref().unwrap_or_default()), truncated: false, killed }
}

fn count_newlines(file: &Path, size: u64) -> Option<u64> {
    if size == 0 {
        return Some(0);
    }
    if size > COUNT_NEWLINES_LIMIT {
        return None;
    }
    let Ok(handle) = File::open(file) else {
        return Some(0);
    };
    let mut handle = handle.take(size);
    let mut count = 0;
    let mut buffer = [0u8; 65536];
    loop {
        match handle.read(&mut buffer) {
            Ok(0) => return Some(count),
            Ok(read) => count += buffer[..read].iter().filter(|byte| **byte == b'\n').count() as u64,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Some(count),
        }
    }
}

fn scan_line_starts(file: &Path, size: u64) -> [Option<u64>; 2] {
    let mut starts = Vec::new();
    let floor = size.saturating_sub(LINE_START_SCAN_LIMIT);
    let mut end = size;
    while end > floor && starts.len() < 2 {
        let from = floor.max(end.saturating_sub(65536));
        let chunk = read_range(file, from, end - from);
        for index in (0..chunk.len()).rev() {
            if chunk[index] != b'\n' {
                continue;
            }
            starts.push(from + index as u64 + 1);
            if starts.len() >= 2 {
                break;
            }
        }
        end = from;
    }
    let unknown = (floor == 0).then_some(0);
    [starts.get(1).copied().or(unknown), starts.first().copied().or(unknown)]
}

fn read_range(file: &Path, start: u64, length: u64) -> Vec<u8> {
    if length == 0 {
        return Vec::new();
    }
    let Ok(mut handle) = File::open(file) else {
        return Vec::new();
    };
    if handle.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut bytes = vec![0; usize::try_from(length).unwrap_or(usize::MAX)];
    let mut read = 0;
    while read < bytes.len() {
        match handle.read(&mut bytes[read..]) {
            Ok(0) => break,
            Ok(count) => read += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    bytes.truncate(read);
    bytes
}

fn utf8_tail(bytes: &[u8], limit: u64) -> String {
    if bytes.len() as u64 <= limit {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let start = bytes.len() - usize::try_from(limit).unwrap_or(usize::MAX);
    let end = bytes.len().min(start.saturating_add(3));
    let mut index = start;
    while index < end && bytes[index] & 0xc0 == 0x80 {
        index += 1;
    }
    String::from_utf8_lossy(&bytes[index..]).into_owned()
}

struct Split {
    start: usize,
    end: Option<usize>,
}

fn split_character(bytes: &[u8], cut: usize, running: bool) -> Option<Split> {
    let floor = cut.saturating_sub(3);
    for start in (floor..cut).rev() {
        let lead = bytes[start];
        if lead & 0xc0 == 0x80 {
            continue;
        }
        if !(0xc2..=0xf4).contains(&lead) {
            return None;
        }
        let end = start
            + if lead < 0xe0 {
                2
            } else if lead < 0xf0 {
                3
            } else {
                4
            };
        if end <= cut {
            return None;
        }
        if end <= bytes.len() {
            return std::str::from_utf8(&bytes[start..end]).is_ok().then_some(Split { start, end: Some(end) });
        }
        if running && incomplete_utf8(&bytes[start..]) {
            return Some(Split { start, end: None });
        }
        return None;
    }
    None
}

fn incomplete_utf8(bytes: &[u8]) -> bool {
    let Some((lead, rest)) = bytes.split_first() else {
        return false;
    };
    let expected = if *lead < 0xe0 {
        2
    } else if *lead < 0xf0 {
        3
    } else {
        4
    };
    bytes.len() < expected && rest.iter().all(|byte| byte & 0xc0 == 0x80)
}

fn format_size(bytes: u64) -> String {
    let kb = bytes as f64 / 1024.0;
    if kb > 1024.0 { format!("{:.1} MB", kb / 1024.0) } else { format!("{:.0} KB", kb.round()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencode_util::temp::TempDir;

    #[test]
    fn tail_reports_no_output_for_an_empty_or_missing_file() {
        let capture = tail(
            Path::new("/opencode-missing-output"),
            Tail { size: 0, newlines: Newlines::Known(0), line_starts: LineStarts::Known([0, 0]), stopped_early: None },
            Limits::default(),
            None,
        );
        assert_eq!(capture.output, "(no output)");
        assert!(!capture.truncated);
    }

    #[test]
    fn tail_keeps_the_last_lines_and_reports_their_numbers() {
        let dir = TempDir::new().expect("a temporary directory");
        let file = dir.path().join("out");
        std::fs::write(&file, "a\nb\nc\n").expect("an output file");
        let capture = tail(
            &file,
            Tail { size: 6, newlines: Newlines::Known(3), line_starts: LineStarts::Known([4, 6]), stopped_early: None },
            Limits { max_lines: 2, max_bytes: 1024 },
            None,
        );
        assert!(capture.truncated);
        assert_eq!(
            capture.output,
            format!("b\nc\n\n[showing lines 2-3 of 3; full output saved to {}]", file.display())
        );
    }

    #[test]
    fn format_size_uses_megabytes_above_one_megabyte() {
        assert_eq!(format_size(0), "0 KB");
        assert_eq!(format_size(1536), "2 KB");
        assert_eq!(format_size(2 * 1024 * 1024), "2.0 MB");
    }

    #[test]
    fn count_newlines_reads_only_the_given_size() {
        let dir = TempDir::new().expect("a temporary directory");
        let file = dir.path().join("out");
        std::fs::write(&file, "a\nb\nc\n").expect("an output file");
        assert_eq!(count_newlines(&file, 0), Some(0));
        assert_eq!(count_newlines(&file, 2), Some(1));
        assert_eq!(count_newlines(&file, 4), Some(2));
        assert_eq!(count_newlines(&file, 6), Some(3));
    }
}
