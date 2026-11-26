use std::collections::BTreeMap;

pub const PATCH_CONTEXT_LINES: i32 = 2_147_483_647;
pub const MAX_PATCH_BYTES: usize = 10_000_000;
pub const MAX_TOTAL_PATCH_BYTES: usize = 10_000_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Patch {
    pub text: String,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PatchCount {
    pub additions: usize,
    pub deletions: usize,
}

pub fn empty_patch(file: &str) -> String {
    join_lines(header_lines(file))
}

pub fn add_patch(file: &str, content: &str) -> String {
    one_sided_patch(file, content, '+', |count| format!("@@ -0,0 +1,{count} @@"))
}

pub fn delete_patch(file: &str, content: &str) -> String {
    one_sided_patch(file, content, '-', |count| format!("@@ -1,{count} +0,0 @@"))
}

pub fn count_patch(patch: &str) -> PatchCount {
    let additions = patch.split('\n').filter(|line| line.starts_with('+') && !line.starts_with("+++")).count();
    let deletions = patch.split('\n').filter(|line| line.starts_with('-') && !line.starts_with("---")).count();
    PatchCount { additions, deletions }
}

pub fn file_from_patch_chunk(chunk: &str) -> Option<String> {
    let next = header_value(chunk, "+++ ").and_then(file_from_diff_path);
    let file = next.or_else(|| header_value(chunk, "--- ").and_then(file_from_diff_path));
    if let Some(file) = file.filter(|file| !file.is_empty()) {
        return Some(file);
    }
    file_from_git_header(header_value(chunk, "diff --git ").unwrap_or(""))
}

pub fn split_git_patch(patch: &Patch) -> Vec<&str> {
    let mut starts: Vec<usize> = patch.text.match_indices("\ndiff --git ").map(|(index, _)| index + 1).collect();
    if patch.text.starts_with("diff --git ") {
        starts.insert(0, 0);
    }
    let mut chunks: Vec<&str> = starts
        .iter()
        .enumerate()
        .map(|(index, start)| {
            let end = starts.get(index + 1).copied().unwrap_or(patch.text.len());
            &patch.text[*start..end]
        })
        .collect();
    if patch.truncated {
        chunks.pop();
    }
    chunks
}

pub fn chunks_by_file(patch: &Patch, fallback: impl Fn(usize) -> Option<String>) -> BTreeMap<String, String> {
    split_git_patch(patch).into_iter().enumerate().fold(BTreeMap::new(), |mut files, (index, chunk)| {
        let Some(file) = file_from_patch_chunk(chunk).or_else(|| fallback(index)).filter(|file| !file.is_empty())
        else {
            return files;
        };
        files.entry(file).or_default().push_str(chunk);
        files
    })
}

fn header_lines(file: &str) -> Vec<String> {
    vec![format!("Index: {file}"), "=".repeat(67), format!("--- {file}\t"), format!("+++ {file}\t")]
}

fn one_sided_patch(file: &str, content: &str, marker: char, hunk_header: impl Fn(usize) -> String) -> String {
    let mut lines = header_lines(file);
    if content.is_empty() {
        return join_lines(lines);
    }
    let ends_with_newline = content.ends_with('\n');
    let mut body: Vec<&str> = content.split('\n').collect();
    if ends_with_newline {
        body.pop();
    }
    lines.push(hunk_header(body.len()));
    lines.extend(body.into_iter().map(|line| format!("{marker}{line}")));
    if !ends_with_newline {
        lines.push("\\ No newline at end of file".to_string());
    }
    join_lines(lines)
}

fn join_lines(lines: Vec<String>) -> String {
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

fn header_value<'a>(chunk: &'a str, prefix: &str) -> Option<&'a str> {
    chunk
        .split('\n')
        .flat_map(|line| line.split('\r'))
        .find_map(|line| line.strip_prefix(prefix).filter(|value| !value.is_empty()))
}

fn parse_quoted_path(value: &str) -> Option<(String, usize)> {
    let mut out = String::new();
    let mut chars = value.chars().enumerate().skip(1);
    loop {
        let (index, character) = chars.next()?;
        if character == '"' {
            return Some((out, index + 1));
        }
        if character != '\\' {
            out.push(character);
            continue;
        }
        let (_, escaped) = chars.next()?;
        match escaped {
            't' => out.push('\t'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            other => out.push(other),
        }
    }
}

fn parse_path_token(value: &str) -> String {
    if !value.starts_with('"') {
        return value.split('\t').next().unwrap_or_default().to_string();
    }
    parse_quoted_path(value).map(|(parsed, _)| parsed).unwrap_or_else(|| value.to_string())
}

fn file_from_diff_path(value: &str) -> Option<String> {
    if value.is_empty() || value == "/dev/null" {
        return None;
    }
    let file = parse_path_token(value);
    if let Some(stripped) = file.strip_prefix("a/").or_else(|| file.strip_prefix("b/")) {
        return Some(stripped.to_string());
    }
    Some(file)
}

fn file_from_git_header(header: &str) -> Option<String> {
    if header.starts_with('"') {
        let (_, end) = parse_quoted_path(header)?;
        let rest = header.chars().skip(end).collect::<String>();
        let rest = rest.trim_start();
        if rest.is_empty() {
            return None;
        }
        if !rest.starts_with('"') {
            return file_from_diff_path(rest);
        }
        return parse_quoted_path(rest).and_then(|(value, _)| file_from_diff_path(&value));
    }
    let index = header.find(" b/")?;
    file_from_diff_path(header.get(index + 1..)?)
}
