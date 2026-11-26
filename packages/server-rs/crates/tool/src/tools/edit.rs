use futures_util::future::BoxFuture;
use indexmap::IndexMap;
use opencode_fs::FsError;
use opencode_fs::access::AccessError;
use opencode_fs::diff::{self, FileDiffStatus};
use opencode_fs::mutation::Mutation;
use opencode_fs::write::{FileWriter, WriteError};
use serde_json::{Value, json};

use crate::context::{Content, ToolContext, ToolError, ToolOutput, ToolRuntime, file_invocation, permission_input};
use crate::normalize::normalize_for_match;

const HINT_LINES: usize = 12;
const HINT_LINE_LENGTH: usize = 200;

#[derive(Clone)]
pub struct EditDeps {
    pub writer: FileWriter,
    pub mutation: Mutation,
}

pub struct EditRuntime {
    deps: EditDeps,
}

impl EditRuntime {
    pub fn new(deps: EditDeps) -> Self {
        EditRuntime { deps }
    }

    async fn execute_inner(&self, input: Value, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path = input.get("path").and_then(Value::as_str).unwrap_or_default().to_owned();
        let old = input.get("oldString").and_then(Value::as_str).unwrap_or_default().to_owned();
        let new = input.get("newString").and_then(Value::as_str).unwrap_or_default().to_owned();
        let replace_all = input.get("replaceAll").and_then(Value::as_bool) == Some(true);
        if old == new {
            return Err(ToolError::new("No changes to apply: oldString and newString are identical."));
        }
        if old.is_empty() {
            return Err(ToolError::new("oldString must not be empty. Use write to create or overwrite a file."));
        }
        let lock = self.deps.writer.locate(&path);
        self.deps
            .mutation
            .with_lock(std::slice::from_ref(&lock), self.edit(&path, &old, &new, replace_all, context))
            .await
    }

    async fn edit(
        &self,
        path: &str,
        old: &str,
        new: &str,
        replace_all: bool,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let target = self
            .deps
            .writer
            .target(path, &file_invocation(context))
            .await
            .map_err(|error| access_error(error, path))?;
        if tokio::fs::metadata(&target.absolute).await.is_ok_and(|metadata| metadata.is_dir()) {
            return Err(ToolError::new(format!("Path is a directory, not a file: {path}")));
        }
        let source =
            self.deps.mutation.read_editable_text(&target).await.map_err(|error| read_error(error, path))?.text;
        let endings_only = old.replace("\r", "") == new.replace("\r", "");
        let (matches, replacements) = if endings_only {
            let matches = find_occurrences(&source.chars().collect::<Vec<_>>(), &old.chars().collect::<Vec<_>>());
            let replacements = vec![new.to_owned(); matches.len()];
            (matches, replacements)
        } else {
            match_view(&source, old, new)
        };
        let replacements_count = matches.len();
        let selected = if replace_all { replacements_count } else { replacements_count.min(1) };
        let replaced = apply_matches(&source, &matches[..selected], &replacements[..selected]);
        let preview = (replacements_count > 0 && (replacements_count == 1 || replace_all))
            .then(|| diff::file_diff(&target.resource, &source, &replaced, FileDiffStatus::Modified));
        self.deps
            .writer
            .authorize(
                &target,
                &file_invocation(context),
                preview.map(|preview| serde_json::to_value(preview).expect("the file diff serializes")),
            )
            .await
            .map_err(|error| access_error(error, path))?;
        if replacements_count == 0 {
            let readable = context
                .authorizer
                .allows(permission_input(context, "read", vec![target.resource.clone()], vec!["*".to_owned()]))
                .await
                .unwrap_or(false);
            let hint = readable.then(|| miss_hint(&source, old, new)).flatten();
            let message = format!(
                "Could not find oldString in {path}. It must match exactly, including whitespace and indentation.{}",
                hint.map_or(String::new(), |hint| format!("\n\n{hint}"))
            );
            return Err(ToolError::new(message));
        }
        if replacements_count > 1 && !replace_all {
            return Err(ToolError::new(format!(
                "Found {replacements_count} matches for oldString, but expected exactly one. Add more surrounding context to make oldString unique, or set replaceAll to true to replace every occurrence."
            )));
        }
        if replaced == source {
            return Err(ToolError::new(if endings_only {
                format!("No changes to apply: {path} already has those line endings.")
            } else {
                format!(
                    "No changes to apply: the edit leaves {path} unchanged. Line endings follow the file; to change them, write \\r explicitly in oldString or newString."
                )
            }));
        }
        let written = self.deps.writer.commit(&target, &replaced).await.map_err(|error| commit_error(error, path))?;
        let formatted = match written.formatted {
            Some(formatted) => formatted,
            None => self.deps.mutation.read_text(&target).await.map_err(|error| read_error(error, path))?.text,
        };
        let preview = diff::file_diff(&written.resource, &source, &formatted, FileDiffStatus::Modified);
        let message = format!(
            "Edited {} ({} replacement{})",
            written.resource,
            replacements_count,
            if replacements_count == 1 { "" } else { "s" }
        );
        Ok(ToolOutput {
            output: Value::String(message.clone()),
            content: vec![Content::text(message)],
            metadata: Some(json!({ "files": [serde_json::to_value(preview).expect("the file diff serializes")] })),
        })
    }
}

impl ToolRuntime for EditRuntime {
    fn execute<'a>(&'a self, input: Value, context: &'a ToolContext) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        Box::pin(self.execute_inner(input, context))
    }
}

#[derive(Clone, Copy)]
struct Match {
    start: usize,
    end: usize,
}

fn find_occurrences(content: &[char], search: &[char]) -> Vec<Match> {
    if search.is_empty() {
        return Vec::new();
    }
    let mut result = Vec::new();
    let mut offset = 0;
    while offset + search.len() <= content.len() {
        if content[offset..].starts_with(search) {
            result.push(Match { start: offset, end: offset + search.len() });
            offset += search.len();
        } else {
            offset += 1;
        }
    }
    result
}

fn lf_view(source: &[char]) -> (Vec<char>, Vec<usize>) {
    let mut view = Vec::with_capacity(source.len());
    let mut removed = Vec::new();
    let mut index = 0;
    while index < source.len() {
        if source[index] == '\r' && source.get(index + 1) == Some(&'\n') {
            removed.push(view.len());
            index += 1;
            continue;
        }
        view.push(source[index]);
        index += 1;
    }
    (view, removed)
}

fn match_view(source: &str, old: &str, new: &str) -> (Vec<Match>, Vec<String>) {
    let source_chars = source.chars().collect::<Vec<_>>();
    let (view, removed) = lf_view(&source_chars);
    let file_ending = line_ending(source, "\n");
    let search = replace_crlf(old).chars().collect::<Vec<_>>();
    let replacement = replace_crlf(new);
    let exact = find_occurrences(&view, &search);
    let found = if !exact.is_empty() {
        exact
    } else {
        let view_text = view.iter().collect::<String>();
        let normalized_view = normalize_for_match(&view_text).chars().collect::<Vec<_>>();
        let search_text = search.iter().collect::<String>();
        let normalized_search = normalize_for_match(&search_text).chars().collect::<Vec<_>>();
        let unicode = find_occurrences(&normalized_view, &normalized_search);
        if unicode.is_empty() { find_line_occurrences(&view, &search) } else { unicode }
    };
    let matches = found
        .into_iter()
        .map(|matched| Match {
            start: matched.start + count_before(&removed, matched.start),
            end: matched.end + count_before(&removed, matched.end),
        })
        .collect::<Vec<_>>();
    let replacements = matches
        .iter()
        .map(|matched| {
            let region = source_chars[matched.start..matched.end].iter().collect::<String>();
            with_region_endings(&region, &replacement, &line_ending(&region, &file_ending))
        })
        .collect();
    (matches, replacements)
}

fn find_line_occurrences(view: &[char], search: &[char]) -> Vec<Match> {
    let trailing_newline = search.last() == Some(&'\n');
    let mut expected = split_chars(search, '\n');
    if trailing_newline {
        expected.pop();
    }
    let lines = lines_of(view);
    let mut candidates = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if index + expected.len() > lines.len() {
            break;
        }
        let actual = &lines[index..index + expected.len()];
        if !actual.iter().zip(&expected).all(|(item, expected)| {
            normalize_for_match(item.text.trim_end()) == normalize_for_match(expected.trim_end())
        }) {
            continue;
        }
        let last = actual.last().expect("the expected line list is not empty");
        if trailing_newline && !last.newline {
            continue;
        }
        candidates.push(Match { start: line.start, end: if trailing_newline { last.end } else { last.content_end } });
    }
    let mut result: Vec<Match> = Vec::new();
    for candidate in candidates {
        if result.iter().any(|existing| existing.end > candidate.start && existing.start < candidate.end) {
            continue;
        }
        result.push(candidate);
    }
    result
}

struct Line {
    start: usize,
    end: usize,
    text: String,
    content_end: usize,
    newline: bool,
}

fn lines_of(view: &[char]) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, character) in view.iter().enumerate() {
        if *character != '\n' {
            continue;
        }
        let text = view[start..index].iter().collect::<String>();
        let content_end = start + text.chars().count() - usize::from(text.ends_with('\r'));
        lines.push(Line { start, end: index + 1, text, content_end, newline: true });
        start = index + 1;
    }
    if start < view.len() {
        let text = view[start..].iter().collect::<String>();
        let content_end = start + text.chars().count() - usize::from(text.ends_with('\r'));
        lines.push(Line { start, end: view.len(), text, content_end, newline: false });
    }
    lines
}

fn split_chars(value: &[char], separator: char) -> Vec<String> {
    value.split(|character| *character == separator).map(|line| line.iter().collect()).collect()
}

fn with_region_endings(region: &str, replacement: &str, fallback: &str) -> String {
    let before = split_region_lines(region);
    let endings = region_endings(region);
    let after = replacement.split('\n').map(str::to_owned).collect::<Vec<_>>();
    let source = align_lines(
        &before.iter().map(|line| normalize_for_match(line.trim_end())).collect::<Vec<_>>(),
        &after.iter().map(|line| normalize_for_match(line.trim_end())).collect::<Vec<_>>(),
    );
    let mut result = String::new();
    for (index, line) in after.iter().enumerate() {
        result.push_str(line);
        if index + 1 == after.len() {
            continue;
        }
        let ending = source
            .get(index)
            .copied()
            .flatten()
            .and_then(|source| endings.get(source))
            .map_or(fallback, String::as_str);
        result.push_str(ending);
    }
    result
}

fn split_region_lines(region: &str) -> Vec<String> {
    let characters = region.chars().collect::<Vec<_>>();
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut index = 0;
    while index < characters.len() {
        if characters[index] == '\n' {
            lines.push(std::mem::take(&mut current));
            index += 1;
            continue;
        }
        if characters[index] == '\r' && characters.get(index + 1) == Some(&'\n') {
            index += 1;
            continue;
        }
        current.push(characters[index]);
        index += 1;
    }
    lines.push(current);
    lines
}

fn region_endings(region: &str) -> Vec<String> {
    let characters = region.chars().collect::<Vec<_>>();
    let mut endings = Vec::new();
    for (index, character) in characters.iter().enumerate() {
        if *character != '\n' {
            continue;
        }
        let start = if index > 0 && characters[index - 1] == '\r' { index - 1 } else { index };
        endings.push(characters[start..=index].iter().collect());
    }
    endings
}

fn align_lines(before: &[String], after: &[String]) -> Vec<Option<usize>> {
    let width = after.len() + 1;
    let mut table = vec![0u32; (before.len() + 1) * width];
    for i in (0..before.len()).rev() {
        for j in (0..after.len()).rev() {
            table[i * width + j] = if before[i] == after[j] {
                table[(i + 1) * width + j + 1] + 1
            } else {
                table[(i + 1) * width + j].max(table[i * width + j + 1])
            };
        }
    }
    let mut result = vec![None; after.len()];
    let mut gap_before: Vec<usize> = Vec::new();
    let mut gap_after: Vec<usize> = Vec::new();
    let mut i = 0;
    let mut j = 0;
    while i < before.len() && j < after.len() {
        if before[i] == after[j] {
            flush(&mut result, &mut gap_before, &mut gap_after);
            result[j] = Some(i);
            i += 1;
            j += 1;
            continue;
        }
        if table[(i + 1) * width + j] >= table[i * width + j + 1] {
            gap_before.push(i);
            i += 1;
        } else {
            gap_after.push(j);
            j += 1;
        }
    }
    while i < before.len() {
        gap_before.push(i);
        i += 1;
    }
    while j < after.len() {
        gap_after.push(j);
        j += 1;
    }
    flush(&mut result, &mut gap_before, &mut gap_after);
    result
}

fn flush(result: &mut [Option<usize>], before: &mut Vec<usize>, after: &mut Vec<usize>) {
    for (position, line) in after.iter().enumerate() {
        result[*line] = before.get(position).copied();
    }
    before.clear();
    after.clear();
}

fn apply_matches(source: &str, matches: &[Match], replacements: &[String]) -> String {
    let characters = source.chars().collect::<Vec<_>>();
    let mut result = String::new();
    let mut cursor = 0;
    for (matched, replacement) in matches.iter().zip(replacements) {
        result.push_str(&characters[cursor..matched.start].iter().collect::<String>());
        result.push_str(replacement);
        cursor = matched.end;
    }
    result.push_str(&characters[cursor..].iter().collect::<String>());
    result
}

fn replace_crlf(value: &str) -> String {
    value.replace("\r\n", "\n")
}

fn line_ending(text: &str, fallback: &str) -> String {
    let characters = text.chars().collect::<Vec<_>>();
    let crlf = characters.windows(2).filter(|window| window[0] == '\r' && window[1] == '\n').count();
    let lf = characters.iter().filter(|character| **character == '\n').count() - crlf;
    if crlf == lf {
        fallback.to_owned()
    } else if crlf > lf {
        "\r\n".to_owned()
    } else {
        "\n".to_owned()
    }
}

fn count_before(positions: &[usize], offset: usize) -> usize {
    let mut low = 0;
    let mut high = positions.len();
    while low < high {
        let middle = (low + high) / 2;
        if positions[middle] < offset { low = middle + 1 } else { high = middle }
    }
    low
}

fn miss_hint(source: &str, old: &str, new: &str) -> Option<String> {
    let view = replace_crlf(source);
    let replacement = replace_crlf(new);
    if replacement.trim().chars().count() >= 20
        && normalize_for_match(&view).contains(&normalize_for_match(&replacement))
    {
        return Some("newString is already in the file, so this edit may already be applied.".to_owned());
    }
    let key = |line: &str| normalize_for_match(line).split_whitespace().collect::<Vec<_>>().join(" ");
    let distinct = |line: &str| line.chars().any(char::is_alphanumeric);
    let lines = view.split('\n').collect::<Vec<_>>();
    let search_text = replace_crlf(old);
    let search = search_text.split('\n').collect::<Vec<_>>();
    let mut positions: IndexMap<String, Vec<usize>> = IndexMap::new();
    for (index, line) in lines.iter().enumerate() {
        let value = key(line);
        if !distinct(&value) {
            continue;
        }
        positions.entry(value).or_default().push(index);
    }
    let mut votes: IndexMap<isize, isize> = IndexMap::new();
    let mut wanted = 0;
    for (offset, line) in search.iter().enumerate() {
        let value = key(line);
        if !distinct(&value) {
            continue;
        }
        wanted += 1;
        if let Some(indexes) = positions.get(&value) {
            for index in indexes {
                *votes.entry(*index as isize - offset as isize).or_insert(0) += 1;
            }
        }
    }
    let (start, count) =
        votes.iter().fold((0isize, 0isize), |best, (vote, value)| if *value > best.1 { (*vote, *value) } else { best });
    if count == 0 || count * 2 < wanted {
        return None;
    }
    let mut matched = Vec::new();
    for (offset, line) in search.iter().enumerate() {
        let value = key(line);
        if value.is_empty() {
            continue;
        }
        let index = start + offset as isize;
        if index < 0 || index as usize >= lines.len() {
            continue;
        }
        if key(lines[index as usize]) == value {
            matched.push(index as usize);
        }
    }
    let from = *matched.first()?;
    let to = usize::min(matched[matched.len() - 1] + 1, from + HINT_LINES);
    let mut result = vec!["The closest lines in the file:".to_owned()];
    for (offset, line) in lines[from..to].iter().enumerate() {
        let shown = if line.chars().count() > HINT_LINE_LENGTH {
            format!("{}…", line.chars().take(HINT_LINE_LENGTH).collect::<String>())
        } else {
            (*line).to_owned()
        };
        result.push(format!("{}: {shown}", from + offset + 1));
    }
    Some(result.join("\n"))
}

fn access_error(error: AccessError, path: &str) -> ToolError {
    match error {
        AccessError::Permission(error) => ToolError::permission(error),
        AccessError::Failed { .. } => ToolError::new(format!("Unable to edit {path}")),
    }
}

fn read_error(error: FsError, path: &str) -> ToolError {
    match &error {
        FsError::NotFound { .. } => ToolError::new(format!("File not found: {path}")),
        FsError::InvalidUtf8 { .. } => ToolError::new(format!("Unable to edit {path}: {error}")),
        _ => ToolError::new(format!("Unable to edit {path}")),
    }
}

fn commit_error(error: WriteError, path: &str) -> ToolError {
    match error {
        WriteError::Access(AccessError::Permission(error)) => ToolError::permission(error),
        WriteError::Access(AccessError::Failed { .. }) | WriteError::Fs(_) => {
            ToolError::new(format!("Unable to edit {path}"))
        }
    }
}
