use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Change {
    pub value: String,
    pub count: usize,
    pub added: bool,
    pub removed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Hunk {
    pub old_start: isize,
    pub old_lines: usize,
    pub new_start: isize,
    pub new_lines: usize,
    pub lines: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct StructuredPatch {
    pub old_file_name: String,
    pub new_file_name: String,
    pub hunks: Vec<Hunk>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FileDiffStatus {
    Added,
    Deleted,
    Modified,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct FileDiff {
    pub file: String,
    pub patch: String,
    pub additions: usize,
    pub deletions: usize,
    pub status: FileDiffStatus,
}

pub fn diff_lines(before: &str, after: &str) -> Vec<Change> {
    let old_tokens = tokenize(before);
    let new_tokens = tokenize(after);
    let mut components = Vec::new();
    let mut best_path = HashMap::new();

    let mut initial = Path { old_pos: -1, last: None };
    let mut new_pos = extract_common(&mut components, &mut initial, &new_tokens, &old_tokens, 0);
    best_path.insert(0, initial);
    if best_path[&0].old_pos + 1 >= old_tokens.len() as isize && new_pos + 1 >= new_tokens.len() as isize {
        return build_values(best_path[&0].last, &new_tokens, &old_tokens, &components);
    }

    let old_len = old_tokens.len() as isize;
    let new_len = new_tokens.len() as isize;
    let max_edit_length = old_tokens.len() + new_tokens.len();
    let mut min_diagonal = isize::MIN;
    let mut max_diagonal = isize::MAX;

    let mut edit_length = 1usize;
    while edit_length <= max_edit_length {
        let edit = edit_length as isize;
        let mut diagonal = min_diagonal.max(-edit);
        let end = max_diagonal.min(edit);
        while diagonal <= end {
            let remove_path = best_path.remove(&(diagonal - 1));
            let add_path = best_path.get(&(diagonal + 1)).copied();

            let can_add = match add_path {
                Some(path) => {
                    let add_new_pos = path.old_pos - diagonal;
                    0 <= add_new_pos && add_new_pos < new_len
                }
                None => false,
            };
            let can_remove = match remove_path {
                Some(path) => path.old_pos + 1 < old_len,
                None => false,
            };

            if !can_add && !can_remove {
                best_path.remove(&diagonal);
                diagonal += 2;
                continue;
            }

            let mut base = if !can_remove
                || (can_add
                    && remove_path.expect("remove path exists").old_pos < add_path.expect("add path exists").old_pos)
            {
                add_to_path(&mut components, add_path.expect("add path exists"), true, false, 0)
            } else {
                add_to_path(&mut components, remove_path.expect("remove path exists"), false, true, 1)
            };

            new_pos = extract_common(&mut components, &mut base, &new_tokens, &old_tokens, diagonal);
            if base.old_pos + 1 >= old_len && new_pos + 1 >= new_len {
                return build_values(base.last, &new_tokens, &old_tokens, &components);
            }

            best_path.insert(diagonal, base);
            if base.old_pos + 1 >= old_len {
                max_diagonal = max_diagonal.min(diagonal - 1);
            }
            if new_pos + 1 >= new_len {
                min_diagonal = min_diagonal.max(diagonal + 1);
            }
            diagonal += 2;
        }
        edit_length += 1;
    }

    build_values(best_path.get(&0).and_then(|path| path.last), &new_tokens, &old_tokens, &components)
}

pub fn structured_patch(old_name: &str, new_name: &str, before: &str, after: &str) -> StructuredPatch {
    const CONTEXT: usize = 4;

    let changes = diff_lines(before, after);
    let mut items = changes
        .into_iter()
        .map(|change| Item { lines: split_lines(&change.value), added: change.added, removed: change.removed })
        .collect::<Vec<_>>();
    items.push(Item { lines: Vec::new(), added: false, removed: false });

    let mut hunks = Vec::new();
    let mut old_range_start = 0isize;
    let mut new_range_start = 0isize;
    let mut current_range = Vec::new();
    let mut old_line = 1isize;
    let mut new_line = 1isize;

    for index in 0..items.len() {
        let current = &items[index];
        if current.added || current.removed {
            if old_range_start == 0 {
                old_range_start = old_line;
                new_range_start = new_line;
                if index > 0 {
                    let previous = &items[index - 1];
                    current_range =
                        context_lines(previous.lines.iter().skip(previous.lines.len().saturating_sub(CONTEXT)));
                    old_range_start -= current_range.len() as isize;
                    new_range_start -= current_range.len() as isize;
                }
            }
            current_range.extend(current.lines.iter().map(|line| {
                let marker = if current.added { '+' } else { '-' };
                format!("{marker}{line}")
            }));
            if current.added {
                new_line += current.lines.len() as isize;
            } else {
                old_line += current.lines.len() as isize;
            }
            continue;
        }

        if old_range_start != 0 {
            if current.lines.len() <= CONTEXT * 2 && index < items.len() - 2 {
                current_range.extend(context_lines(current.lines.iter()));
            } else {
                let context_size = current.lines.len().min(CONTEXT);
                current_range.extend(context_lines(current.lines.iter().take(context_size)));
                hunks.push(Hunk {
                    old_start: old_range_start,
                    old_lines: (old_line - old_range_start + context_size as isize) as usize,
                    new_start: new_range_start,
                    new_lines: (new_line - new_range_start + context_size as isize) as usize,
                    lines: std::mem::take(&mut current_range),
                });
                old_range_start = 0;
                new_range_start = 0;
            }
        }
        old_line += current.lines.len() as isize;
        new_line += current.lines.len() as isize;
    }

    for hunk in &mut hunks {
        let mut index = 0;
        while index < hunk.lines.len() {
            if hunk.lines[index].ends_with('\n') {
                hunk.lines[index].pop();
            } else {
                hunk.lines.insert(index + 1, "\\ No newline at end of file".to_owned());
                index += 1;
            }
            index += 1;
        }
    }

    StructuredPatch { old_file_name: old_name.to_owned(), new_file_name: new_name.to_owned(), hunks }
}

pub fn create_two_files_patch(old_name: &str, new_name: &str, before: &str, after: &str) -> String {
    format_patch(&structured_patch(old_name, new_name, before, after))
}

pub fn file_diff(file: &str, before: &str, after: &str, status: FileDiffStatus) -> FileDiff {
    let mut additions = 0;
    let mut deletions = 0;
    for change in diff_lines(before, after) {
        if change.added {
            additions += change.count;
        }
        if change.removed {
            deletions += change.count;
        }
    }
    FileDiff {
        file: file.to_owned(),
        patch: create_two_files_patch(file, file, before, after),
        additions,
        deletions,
        status,
    }
}

pub fn trim_diff(diff: &str) -> String {
    let lines = diff.split('\n').collect::<Vec<_>>();
    let content = lines.iter().copied().filter(|line| is_patch_content(line)).collect::<Vec<_>>();
    if content.is_empty() {
        return diff.to_owned();
    }
    let mut indent = usize::MAX;
    for line in &content {
        let value = &line[1..];
        if trim_js(value).is_empty() {
            continue;
        }
        indent = indent.min(value.chars().take_while(|character| is_js_space(*character)).count());
    }
    if indent == usize::MAX || indent == 0 {
        return diff.to_owned();
    }
    lines
        .into_iter()
        .map(|line| {
            if !is_patch_content(line) {
                return line.to_owned();
            }
            let mut characters = line.chars();
            let marker = characters.next().expect("patch content starts with a marker");
            format!("{marker}{}", characters.skip(indent).collect::<String>())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_patch_content(line: &str) -> bool {
    (line.starts_with('+') || line.starts_with('-') || line.starts_with(' '))
        && !line.starts_with("---")
        && !line.starts_with("+++")
}

fn is_js_space(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'..='\u{000d}'
            | '\u{0020}'
            | '\u{00a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

fn trim_js(value: &str) -> &str {
    value.trim_matches(is_js_space)
}

fn format_patch(patch: &StructuredPatch) -> String {
    let mut lines = Vec::new();
    if patch.old_file_name == patch.new_file_name {
        lines.push(format!("Index: {}", patch.old_file_name));
    }
    lines.push("===================================================================".to_owned());
    lines.push(format!("--- {}", patch.old_file_name));
    lines.push(format!("+++ {}", patch.new_file_name));
    for hunk in &patch.hunks {
        let old_start = if hunk.old_lines == 0 { hunk.old_start - 1 } else { hunk.old_start };
        let new_start = if hunk.new_lines == 0 { hunk.new_start - 1 } else { hunk.new_start };
        lines.push(format!("@@ -{},{} +{},{} @@", old_start, hunk.old_lines, new_start, hunk.new_lines));
        lines.extend(hunk.lines.iter().cloned());
    }
    format!("{}\n", lines.join("\n"))
}

fn context_lines<'a>(lines: impl Iterator<Item = &'a String>) -> Vec<String> {
    lines.map(|line| format!(" {line}")).collect()
}

fn split_lines(text: &str) -> Vec<String> {
    let has_trailing_newline = text.ends_with('\n');
    let mut result = text.split('\n').map(|line| format!("{line}\n")).collect::<Vec<_>>();
    if has_trailing_newline {
        result.pop();
    } else {
        let mut last = result.pop().unwrap_or_default();
        last.pop();
        result.push(last);
    }
    result
}

struct Item {
    lines: Vec<String>,
    added: bool,
    removed: bool,
}

#[derive(Clone, Copy)]
struct Path {
    old_pos: isize,
    last: Option<usize>,
}

struct Component {
    count: usize,
    added: bool,
    removed: bool,
    previous: Option<usize>,
}

fn add_to_path(components: &mut Vec<Component>, path: Path, added: bool, removed: bool, old_pos_inc: isize) -> Path {
    let extend = path.last.filter(|last| {
        let component = &components[*last];
        component.added == added && component.removed == removed
    });
    components.push(match extend {
        Some(last) => {
            let component = &components[last];
            Component { count: component.count + 1, added, removed, previous: component.previous }
        }
        None => Component { count: 1, added, removed, previous: path.last },
    });
    Path { old_pos: path.old_pos + old_pos_inc, last: Some(components.len() - 1) }
}

fn extract_common(
    components: &mut Vec<Component>,
    base: &mut Path,
    new_tokens: &[String],
    old_tokens: &[String],
    diagonal: isize,
) -> isize {
    let new_len = new_tokens.len() as isize;
    let old_len = old_tokens.len() as isize;
    let mut old_pos = base.old_pos;
    let mut new_pos = old_pos - diagonal;
    let mut common = 0;
    while new_pos + 1 < new_len
        && old_pos + 1 < old_len
        && old_tokens[(old_pos + 1) as usize] == new_tokens[(new_pos + 1) as usize]
    {
        new_pos += 1;
        old_pos += 1;
        common += 1;
    }
    if common > 0 {
        components.push(Component { count: common, added: false, removed: false, previous: base.last });
        base.last = Some(components.len() - 1);
    }
    base.old_pos = old_pos;
    new_pos
}

fn build_values(
    last: Option<usize>,
    new_tokens: &[String],
    old_tokens: &[String],
    components: &[Component],
) -> Vec<Change> {
    let mut ordered = Vec::new();
    let mut current = last;
    while let Some(index) = current {
        ordered.push(index);
        current = components[index].previous;
    }
    ordered.reverse();

    let mut changes = Vec::with_capacity(ordered.len());
    let mut new_pos = 0;
    let mut old_pos = 0;
    for index in ordered {
        let component = &components[index];
        let value = if component.removed {
            let value = old_tokens[old_pos..old_pos + component.count].concat();
            old_pos += component.count;
            value
        } else {
            let value = new_tokens[new_pos..new_pos + component.count].concat();
            new_pos += component.count;
            if !component.added {
                old_pos += component.count;
            }
            value
        };
        changes.push(Change { value, count: component.count, added: component.added, removed: component.removed });
    }
    changes
}

fn tokenize(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\n' {
            current.push('\n');
            tokens.push(std::mem::take(&mut current));
            index += 1;
        } else if bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
            current.push_str("\r\n");
            tokens.push(std::mem::take(&mut current));
            index += 2;
        } else {
            let character = value[index..].chars().next().expect("index is a char boundary");
            current.push(character);
            index += character.len_utf8();
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}
