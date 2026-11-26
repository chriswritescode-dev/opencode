use std::fmt;

use opencode_fs::bom;
use serde::{Deserialize, Serialize};

use crate::normalize::normalize_for_match;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateFileChunk {
    pub old_lines: Vec<String>,
    pub new_lines: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_context: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub end_of_file: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Hunk {
    Add {
        path: String,
        contents: String,
    },
    Delete {
        path: String,
    },
    Update {
        path: String,
        #[serde(rename = "movePath", default, skip_serializing_if = "Option::is_none")]
        move_path: Option<String>,
        chunks: Vec<UpdateFileChunk>,
    },
}

impl Hunk {
    pub fn path(&self) -> &str {
        match self {
            Hunk::Add { path, .. } | Hunk::Delete { path } | Hunk::Update { path, .. } => path,
        }
    }

    pub fn move_path(&self) -> Option<&str> {
        match self {
            Hunk::Update { move_path, .. } => move_path.as_deref(),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseError {
    Boundary { boundary: &'static str },
    InvalidHunk { line: String, line_number: usize, reason: Option<String> },
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Boundary { boundary } => write!(
                formatter,
                "The {boundary} line of the patch must be '{}'",
                if *boundary == "first" { "*** Begin Patch" } else { "*** End Patch" }
            ),
            ParseError::InvalidHunk { line, line_number, reason } => match reason {
                Some(reason) => write!(formatter, "Invalid hunk at line {line_number}: {reason}"),
                None => write!(
                    formatter,
                    "Invalid hunk at line {line_number}: '{line}' is not a valid hunk header. Valid hunk headers: '*** Add File: {{path}}', '*** Delete File: {{path}}', '*** Update File: {{path}}'"
                ),
            },
        }
    }
}

impl std::error::Error for ParseError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileUpdate {
    pub content: String,
    pub bom: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeriveError(pub String);

impl fmt::Display for DeriveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for DeriveError {}

pub fn parse(patch_text: &str) -> Result<Vec<Hunk>, ParseError> {
    let lines = strip_heredoc(patch_text.trim())
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned())
        .collect::<Vec<_>>();
    if !lines.first().is_some_and(|line| line.trim() == "*** Begin Patch") {
        return Err(ParseError::Boundary { boundary: "first" });
    }
    let end = match lines.last() {
        Some(line) if line.trim() == "*** End Patch" => lines.len() - 1,
        _ => return Err(ParseError::Boundary { boundary: "last" }),
    };
    if end == 0 {
        return Err(ParseError::Boundary { boundary: "last" });
    }

    let mut hunks = Vec::new();
    let mut index = 1;
    while index < end {
        let line = &lines[index];
        let header = line.trim();
        if index == 1
            && header.starts_with("*** Environment ID:")
            && !header["*** Environment ID:".len()..].trim().is_empty()
        {
            index += 1;
            continue;
        }
        if let Some(rest) = header.strip_prefix("*** Add File: ") {
            let path = rest.trim();
            let (content, next) = parse_add(&lines, index + 1, end, path)?;
            hunks.push(Hunk::Add { path: path.to_owned(), contents: content });
            index = next;
            continue;
        }
        if let Some(rest) = header.strip_prefix("*** Delete File: ") {
            let path = rest.trim();
            let next = lines.get(index + 1).map(|line| line.trim());
            if index + 1 < end && next.is_some_and(|next| !is_boundary(next)) {
                let next = next.unwrap_or_default();
                if next.starts_with("*** ") {
                    return Err(ParseError::InvalidHunk {
                        line: next.to_owned(),
                        line_number: index + 2,
                        reason: None,
                    });
                }
                return Err(ParseError::InvalidHunk {
                    line: next.to_owned(),
                    line_number: index + 2,
                    reason: Some(format!(
                        "Unexpected line after Delete File '{path}': '{next}'. Delete hunks do not contain body lines"
                    )),
                });
            }
            hunks.push(Hunk::Delete { path: path.to_owned() });
            index += 1;
            continue;
        }
        if let Some(rest) = header.strip_prefix("*** Update File: ") {
            let path = rest.trim();
            let mut next = index + 1;
            while lines.get(next).is_some_and(|line| line.trim_end() == "*** End of File") {
                next += 1;
            }
            let mut move_path = None;
            let candidate = lines.get(next).map(|line| line.trim_end()).unwrap_or_default();
            if candidate == "*** Move to:" || candidate.starts_with("*** Move to: ") {
                let destination = candidate.strip_prefix("*** Move to: ").unwrap_or_default().trim();
                if destination.is_empty() {
                    return Err(ParseError::InvalidHunk {
                        line: lines[next].trim().to_owned(),
                        line_number: next + 1,
                        reason: Some(format!("Move destination for '{path}' must not be empty")),
                    });
                }
                move_path = Some(destination.to_owned());
                next += 1;
            }
            let (chunks, next) = parse_update(&lines, next, end, path, index)?;
            hunks.push(Hunk::Update { path: path.to_owned(), move_path, chunks });
            index = next;
            continue;
        }
        return Err(ParseError::InvalidHunk { line: header.to_owned(), line_number: index + 1, reason: None });
    }
    Ok(hunks)
}

fn parse_add(lines: &[String], start: usize, end: usize, path: &str) -> Result<(String, usize), ParseError> {
    let mut content = Vec::new();
    let mut index = start;
    while index < end && !is_boundary(lines[index].trim()) {
        let line = &lines[index];
        if !line.starts_with('+') {
            let trimmed = line.trim();
            return Err(ParseError::InvalidHunk {
                line: trimmed.to_owned(),
                line_number: index + 1,
                reason: Some(format!(
                    "Invalid Add File line for '{path}': expected a line starting with '+', got '{trimmed}'"
                )),
            });
        }
        content.push(line[1..].to_owned());
        index += 1;
    }
    Ok((content.join("\n"), index))
}

fn parse_update(
    lines: &[String],
    start: usize,
    end: usize,
    path: &str,
    hunk: usize,
) -> Result<(Vec<UpdateFileChunk>, usize), ParseError> {
    let mut chunks: Vec<UpdateFileChunk> = Vec::new();
    let mut index = start;
    let mut after_end_of_file = false;
    while index < end {
        let line = &lines[index];
        let update_line = line.trim_end();
        if after_end_of_file {
            if update_line.is_empty() {
                index += 1;
                continue;
            }
            if update_line == "@@" || update_line.starts_with("@@ ") {
                after_end_of_file = false;
            } else if is_boundary(update_line) {
                break;
            } else {
                return Err(ParseError::InvalidHunk {
                    line: line.clone(),
                    line_number: index + 1,
                    reason: Some(format!("Expected update hunk to start with a @@ context marker, got: '{line}'")),
                });
            }
        }
        if update_line == "*** End of File" {
            match chunks.last_mut() {
                Some(chunk) if chunk.old_lines.is_empty() && chunk.new_lines.is_empty() => {
                    return Err(ParseError::InvalidHunk {
                        line: update_line.to_owned(),
                        line_number: index + 1,
                        reason: Some("Update hunk does not contain any lines".to_owned()),
                    });
                }
                Some(chunk) => {
                    chunk.end_of_file = true;
                    after_end_of_file = true;
                }
                None => {}
            }
            index += 1;
            continue;
        }
        if is_boundary(update_line) {
            break;
        }
        if update_line == "@@" || update_line.starts_with("@@ ") {
            if chunks.last().is_some_and(|chunk| chunk.old_lines.is_empty() && chunk.new_lines.is_empty()) {
                return Err(ParseError::InvalidHunk {
                    line: line.clone(),
                    line_number: index + 1,
                    reason: Some(format!(
                        "Unexpected line found in update hunk: '{line}'. Every line should start with ' ' (context line), '+' (added line), or '-' (removed line)"
                    )),
                });
            }
            chunks.push(UpdateFileChunk {
                old_lines: Vec::new(),
                new_lines: Vec::new(),
                change_context: (update_line != "@@").then(|| update_line["@@ ".len()..].to_owned()),
                end_of_file: false,
            });
            index += 1;
            continue;
        }
        if chunks.is_empty() {
            chunks.push(UpdateFileChunk {
                old_lines: Vec::new(),
                new_lines: Vec::new(),
                change_context: None,
                end_of_file: false,
            });
        }
        let chunk = chunks.last_mut().expect("the update hunk has a chunk");
        if line.is_empty() {
            chunk.old_lines.push(String::new());
            chunk.new_lines.push(String::new());
            index += 1;
            continue;
        }
        if let Some(rest) = line.strip_prefix(' ') {
            chunk.old_lines.push(rest.to_owned());
            chunk.new_lines.push(rest.to_owned());
            index += 1;
            continue;
        }
        if let Some(rest) = line.strip_prefix('-') {
            chunk.old_lines.push(rest.to_owned());
            index += 1;
            continue;
        }
        if let Some(rest) = line.strip_prefix('+') {
            chunk.new_lines.push(rest.to_owned());
            index += 1;
            continue;
        }
        let populated = !chunk.old_lines.is_empty() || !chunk.new_lines.is_empty();
        return Err(ParseError::InvalidHunk {
            line: line.clone(),
            line_number: index + 1,
            reason: Some(if populated {
                format!("Expected update hunk to start with a @@ context marker, got: '{line}'")
            } else {
                format!(
                    "Unexpected line found in update hunk: '{line}'. Every line should start with ' ' (context line), '+' (added line), or '-' (removed line)"
                )
            }),
        });
    }
    if chunks.is_empty() {
        return Err(ParseError::InvalidHunk {
            line: lines[hunk].trim().to_owned(),
            line_number: hunk + 1,
            reason: Some(format!("Update file hunk for path '{path}' is empty")),
        });
    }
    if chunks.last().is_some_and(|chunk| chunk.old_lines.is_empty() && chunk.new_lines.is_empty()) {
        let line = lines.get(index).map(|line| line.trim()).unwrap_or_default().to_owned();
        return Err(ParseError::InvalidHunk {
            line_number: index + 1,
            reason: Some(if line == "*** End Patch" {
                "Update hunk does not contain any lines".to_owned()
            } else {
                format!(
                    "Unexpected line found in update hunk: '{line}'. Every line should start with ' ' (context line), '+' (added line), or '-' (removed line)"
                )
            }),
            line,
        });
    }
    Ok((chunks, index))
}

fn is_boundary(line: &str) -> bool {
    line == "*** End Patch"
        || line.starts_with("*** Add File: ")
        || line.starts_with("*** Delete File: ")
        || line.starts_with("*** Update File: ")
}

pub fn derive(path: &str, chunks: &[UpdateFileChunk], original: &str) -> Result<FileUpdate, DeriveError> {
    let (source_bom, source_text) = bom::split(original);
    let mut lines = source_text.split('\n').map(str::to_owned).collect::<Vec<_>>();
    if lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    for replacement in compute_replacements(&lines, path, chunks)?.into_iter().rev() {
        lines.splice(replacement.start..replacement.start + replacement.remove, replacement.insert);
    }
    if !lines.last().is_some_and(|line| line.is_empty()) {
        lines.push(String::new());
    }
    let joined = lines.join("\n");
    let (next_bom, next_text) = bom::split(&joined);
    Ok(FileUpdate { content: next_text.to_owned(), bom: source_bom || next_bom })
}

struct Replacement {
    start: usize,
    remove: usize,
    insert: Vec<String>,
}

fn compute_replacements(
    lines: &[String],
    path: &str,
    chunks: &[UpdateFileChunk],
) -> Result<Vec<Replacement>, DeriveError> {
    let mut replacements = Vec::new();
    let mut line_index = 0;
    for chunk in chunks {
        if let Some(context) = &chunk.change_context {
            let Some(found) = seek(lines, std::slice::from_ref(context), line_index, false) else {
                return Err(DeriveError(format!("Failed to find context '{context}' in {path}")));
            };
            line_index = found + 1;
        }
        if chunk.old_lines.is_empty() {
            replacements.push(Replacement { start: lines.len(), remove: 0, insert: chunk.new_lines.clone() });
            continue;
        }
        let mut old_lines = chunk.old_lines.clone();
        let mut new_lines = chunk.new_lines.clone();
        let mut found = seek(lines, &old_lines, line_index, chunk.end_of_file);
        if found.is_none() && old_lines.last().is_some_and(|line| line.is_empty()) {
            old_lines.pop();
            if new_lines.last().is_some_and(|line| line.is_empty()) {
                new_lines.pop();
            }
            found = seek(lines, &old_lines, line_index, chunk.end_of_file);
        }
        let Some(start) = found else {
            if chunk.old_lines.iter().all(|line| line.is_empty()) {
                let expected = if chunk.old_lines.len() == 1 {
                    "an expected blank line".to_owned()
                } else {
                    format!("{} consecutive blank lines", chunk.old_lines.len())
                };
                return Err(DeriveError(format!("Failed to find {expected} in {path}")));
            }
            return Err(DeriveError(format!(
                "Failed to find expected lines in {path}:\n{}",
                chunk.old_lines.join("\n")
            )));
        };
        replacements.push(Replacement { start, remove: old_lines.len(), insert: new_lines });
        line_index = start + old_lines.len();
    }
    replacements.sort_by_key(|replacement| replacement.start);
    Ok(replacements)
}

fn seek(lines: &[String], pattern: &[String], start: usize, eof: bool) -> Option<usize> {
    if pattern.is_empty() {
        return None;
    }
    let compare: [fn(&str, &str) -> bool; 4] = [exact, rstrip, trim, normalized];
    if eof {
        let offset = lines.len().checked_sub(pattern.len())?;
        if offset < start {
            return None;
        }
        return compare.iter().any(|compare| matches(lines, pattern, offset, *compare)).then_some(offset);
    }
    if start + pattern.len() > lines.len() {
        return None;
    }
    for compare in compare {
        for offset in start..=(lines.len() - pattern.len()) {
            if matches(lines, pattern, offset, compare) {
                return Some(offset);
            }
        }
    }
    None
}

fn matches(lines: &[String], pattern: &[String], offset: usize, compare: fn(&str, &str) -> bool) -> bool {
    pattern.iter().enumerate().all(|(index, line)| compare(&lines[offset + index], line))
}

fn exact(left: &str, right: &str) -> bool {
    left == right
}

fn rstrip(left: &str, right: &str) -> bool {
    left.trim_end() == right.trim_end()
}

fn trim(left: &str, right: &str) -> bool {
    left.trim() == right.trim()
}

fn normalized(left: &str, right: &str) -> bool {
    normalize_for_match(left.trim()) == normalize_for_match(right.trim())
}

fn strip_heredoc(input: &str) -> &str {
    let rest = match input.strip_prefix("cat") {
        Some(rest) => {
            let trimmed = rest.trim_start();
            if trimmed.len() == rest.len() {
                return input;
            }
            trimmed
        }
        None => input,
    };
    let Some(rest) = rest.strip_prefix("<<") else { return input };
    let (quote, rest) = match rest.chars().next() {
        Some(quote @ ('\'' | '"')) => (Some(quote), &rest[quote.len_utf8()..]),
        _ => (None, rest),
    };
    let length =
        rest.find(|character: char| !(character.is_ascii_alphanumeric() || character == '_')).unwrap_or(rest.len());
    if length == 0 {
        return input;
    }
    let word = &rest[..length];
    let rest = &rest[length..];
    let rest = match quote {
        Some(quote) => match rest.strip_prefix(quote) {
            Some(rest) => rest,
            None => return input,
        },
        None => rest,
    };
    let Some(body) = heredoc_body(rest) else { return input };
    let mut from = 0;
    while let Some(found) = body[from..].find('\n') {
        let newline = from + found;
        let after = &body[newline + 1..];
        if let Some(remainder) = after.strip_prefix(word)
            && remainder.trim().is_empty()
        {
            return &body[..newline];
        }
        from = newline + 1;
    }
    input
}

fn heredoc_body(rest: &str) -> Option<&str> {
    let trimmed = rest.trim_start();
    let leading = rest.len() - trimmed.len();
    if leading == 0 || !rest[..leading].ends_with('\n') {
        return None;
    }
    Some(trimmed)
}

fn is_false(value: &bool) -> bool {
    !*value
}
