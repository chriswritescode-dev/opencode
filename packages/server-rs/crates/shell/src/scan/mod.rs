mod bash;
mod bash_grammar;
mod bash_words;
mod powershell;

pub use powershell::scan_power_shell;

use regress::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::LazyLock;

pub const MAX_INPUT_LENGTH: usize = 64 * 1024;
pub const MAX_SUBSTITUTION_DEPTH: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dialect {
    Bash,
    Zsh,
    Posix,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OpaqueReason {
    CommandSubstitution,
    CompoundCommand,
    DynamicCommandName,
    DynamicExecution,
    Heredoc,
    InvalidRedirect,
    InvalidStructure,
    UnsupportedEscape,
    UnterminatedEscape,
    UnterminatedQuote,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Command {
    pub resource: String,
    pub words: Vec<String>,
    pub raw_words: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub word_ends: Option<Vec<usize>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub statement_head: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declaration: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redirect_word_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redirect_prefix_end: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ScanResult {
    Scanned { commands: Vec<Command> },
    Opaque { reason: OpaqueReason },
}

pub fn scan(input: &str, dialect: Dialect) -> ScanResult {
    let units: Vec<u16> = input.encode_utf16().collect();
    if units.len() > MAX_INPUT_LENGTH {
        return ScanResult::Opaque { reason: OpaqueReason::InvalidStructure };
    }
    let mut context = BashContext::new(dialect);
    let mut heredocs = Vec::new();
    let result = bash::scan_bash(&units, 0, 0, &mut context, None, &mut heredocs, false, None);
    match result {
        BashResult::Opaque(reason) => ScanResult::Opaque { reason },
        BashResult::Scanned { commands, .. } => {
            if context.bound_dollar
                && context.bound_open
                && (context.eval_sink || ((context.bound_bracket || context.sink_bracket) && context.variable_sink))
            {
                return ScanResult::Opaque { reason: OpaqueReason::DynamicExecution };
            }
            ScanResult::Scanned { commands }
        }
    }
}

#[derive(Debug)]
pub(crate) enum BashResult {
    Scanned { commands: Vec<Command>, end: usize },
    Opaque(OpaqueReason),
}

pub(crate) type Step = Result<Option<usize>, OpaqueReason>;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArrayMode {
    Array,
    Pattern,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Close {
    Paren,
    Brace,
    Nofork,
}

pub(crate) struct BashContext {
    pub remaining: i64,
    pub dialect: Dialect,
    pub bound_dollar: bool,
    pub bound_open: bool,
    pub bound_bracket: bool,
    pub sink_bracket: bool,
    pub variable_sink: bool,
    pub eval_sink: bool,
}

impl BashContext {
    fn new(dialect: Dialect) -> Self {
        Self {
            remaining: (MAX_INPUT_LENGTH * MAX_SUBSTITUTION_DEPTH) as i64,
            dialect,
            bound_dollar: false,
            bound_open: false,
            bound_bracket: false,
            sink_bracket: false,
            variable_sink: false,
            eval_sink: false,
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct BashText {
    pub word: String,
    pub literal: String,
    pub output: String,
}

pub(crate) struct Pattern {
    pub(crate) search: Regex,
    sticky: Regex,
}

impl Pattern {
    pub(crate) fn new(pattern: &str) -> Self {
        Self::build(pattern, "")
    }

    pub(crate) fn with_flags(pattern: &str, flags: &str) -> Self {
        Self::build(pattern, flags)
    }

    fn build(pattern: &str, flags: &str) -> Self {
        let search = Regex::with_flags(pattern, flags).expect("valid shell pattern");
        let sticky = Regex::with_flags(&format!("^(?:{pattern})"), flags).expect("valid anchored shell pattern");
        Self { search, sticky }
    }

    pub(crate) fn at(&self, input: &[u16], index: usize) -> Option<std::ops::Range<usize>> {
        if index > input.len() {
            return None;
        }
        let matched = self.sticky.find_from_utf16(&input[index..], 0).next()?;
        Some(index + matched.range.start..index + matched.range.end)
    }

    pub(crate) fn test_at(&self, input: &[u16], index: usize) -> bool {
        self.at(input, index).is_some()
    }

    pub(crate) fn end_at(&self, input: &[u16], index: usize) -> Option<usize> {
        self.at(input, index).map(|range| range.end)
    }

    pub(crate) fn matched_at(&self, input: &[u16], index: usize) -> Option<String> {
        self.at(input, index).map(|range| units(input, range.start, range.end))
    }

    pub(crate) fn is_match(&self, input: &[u16]) -> bool {
        self.search.find_from_utf16(input, 0).next().is_some()
    }

    pub(crate) fn is_match_str(&self, text: &str) -> bool {
        self.is_match(&encode(text))
    }

    pub(crate) fn replace_all(&self, input: &[u16], replacement: &str) -> String {
        let mut output = String::new();
        let mut copied = 0;
        let mut search = 0;
        while let Some(matched) = self.search.find_from_utf16(input, search).next() {
            let range = matched.range;
            output.push_str(&units(input, copied, range.start));
            output.push_str(replacement);
            copied = range.end;
            search = if range.end == range.start { range.end + 1 } else { range.end };
        }
        output.push_str(&units(input, copied, input.len()));
        output
    }
}

pub(crate) fn encode(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

pub(crate) fn units(input: &[u16], start: usize, end: usize) -> String {
    if start >= end || start >= input.len() {
        return String::new();
    }
    String::from_utf16_lossy(&input[start..end.min(input.len())])
}

pub(crate) fn push_unit(text: &mut String, unit: u16) {
    if let Some(ch) = char::from_u32(u32::from(unit)) {
        text.push(ch);
    }
}

pub(crate) fn starts_with(input: &[u16], index: usize, needle: &str) -> bool {
    if needle.is_empty() {
        return index <= input.len();
    }
    for (offset, unit) in needle.encode_utf16().enumerate() {
        if input.get(index + offset) != Some(&unit) {
            return false;
        }
    }
    true
}

pub(crate) fn index_of(input: &[u16], from: usize, needle: &str) -> Option<usize> {
    let needle = encode(needle);
    if needle.is_empty() {
        return Some(from.min(input.len()));
    }
    let tail = input.get(from..)?;
    tail.windows(needle.len()).position(|window| window == needle).map(|at| at + from)
}

pub(crate) fn contains(input: &[u16], from: usize, needle: &str) -> bool {
    index_of(input, from, needle).is_some()
}

pub(crate) fn unit_len(text: &str) -> usize {
    text.encode_utf16().count()
}

pub(crate) fn slice_units(text: &str, start: usize, end: Option<usize>) -> String {
    let input = encode(text);
    let end = end.unwrap_or(input.len());
    units(&input, start, end)
}

pub(crate) fn token_at(input: &[u16], index: usize) -> Option<std::ops::Range<usize>> {
    let first = *input.get(index)?;
    if !matches!(first, 0x41..=0x5a | 0x61..=0x7a | 0x5f) {
        return None;
    }
    let mut cursor = index + 1;
    while input.get(cursor).is_some_and(|unit| matches!(*unit, 0x41..=0x5a | 0x61..=0x7a | 0x30..=0x39 | 0x5f)) {
        cursor += 1;
    }
    let mut after = cursor;
    while input.get(after) == Some(&u16::from(b'\\')) && input.get(after + 1) == Some(&u16::from(b'\n')) {
        after += 2;
    }
    let delimited = input
        .get(after)
        .is_none_or(|unit| matches!(unit, 0x20 | 0x09 | 0x0a | 0x3b | 0x7c | 0x26 | 0x28 | 0x29 | 0x3c | 0x3e));
    delimited.then_some(index..cursor)
}

pub(crate) fn line_continuation_end(input: &[u16], index: usize) -> usize {
    let mut cursor = index;
    while input.get(cursor) == Some(&u16::from(b'\\')) && input.get(cursor + 1) == Some(&u16::from(b'\n')) {
        cursor += 2;
    }
    cursor
}

pub(crate) fn brace_close_ahead(input: &[u16], index: usize) -> bool {
    let cursor = line_continuation_end(input, index);
    input
        .get(cursor)
        .is_none_or(|unit| matches!(*unit, 0x20 | 0x09 | 0x0a | 0x3b | 0x26 | 0x7c | 0x28 | 0x29 | 0x3c | 0x3e))
}

pub(crate) fn space_continuation_ahead(input: &[u16], index: usize) -> bool {
    let cursor = line_continuation_end(input, index);
    input.get(cursor).is_some_and(|unit| matches!(*unit, 0x20 | 0x09 | 0x0a))
}

pub(crate) fn negation_ahead(input: &[u16], index: usize) -> bool {
    let cursor = line_continuation_end(input, index);
    input.get(cursor).is_some_and(|unit| matches!(*unit, 0x20 | 0x09 | 0x0a | 0x28))
}

pub(crate) fn no_fork_open_end(input: &[u16], index: usize) -> Option<usize> {
    if input.get(index) != Some(&u16::from(b'$')) || input.get(index + 1) != Some(&u16::from(b'{')) {
        return None;
    }
    let cursor = line_continuation_end(input, index + 2);
    input.get(cursor).is_some_and(|unit| matches!(*unit, 0x20 | 0x09 | 0x0a | 0x7c)).then_some(cursor + 1)
}

pub(crate) fn zsh_glob_modifier_at(input: &[u16], index: usize) -> bool {
    let mut cursor = index;
    loop {
        match input.get(cursor) {
            Some(&unit) if unit == u16::from(b'~') => return true,
            Some(&0x5e | 0x3d | 0x23 | 0x2b) => cursor += 1,
            Some(&unit) if unit == u16::from(b'\\') && input.get(cursor + 1) == Some(&u16::from(b'\n')) => cursor += 2,
            _ => return false,
        }
    }
}

pub(crate) fn zsh_subscript_end(input: &[u16], index: usize) -> Option<usize> {
    if input.get(index) != Some(&u16::from(b'$')) {
        return None;
    }
    let mut positions = vec![index + 1];
    let mut cursor = index + 1;
    loop {
        if input.get(cursor).is_some_and(|unit| matches!(*unit, 0x5e | 0x3d | 0x2b | 0x7e | 0x23)) {
            cursor += 1;
            positions.push(cursor);
            continue;
        }
        if input.get(cursor) == Some(&u16::from(b'\\')) && input.get(cursor + 1) == Some(&u16::from(b'\n')) {
            cursor += 2;
            positions.push(cursor);
            continue;
        }
        break;
    }
    positions.iter().rev().find_map(|start| zsh_subscript_name(input, *start))
}

fn zsh_subscript_name(input: &[u16], start: usize) -> Option<usize> {
    let mut cursor = start;
    if input.get(cursor).is_some_and(|unit| matches!(*unit, 0x41..=0x5a | 0x61..=0x7a | 0x5f)) {
        cursor += 1;
        loop {
            if input.get(cursor).is_some_and(|unit| matches!(*unit, 0x41..=0x5a | 0x61..=0x7a | 0x30..=0x39 | 0x5f)) {
                cursor += 1;
                continue;
            }
            let next = line_continuation_end(input, cursor);
            if next > cursor {
                cursor = next;
                continue;
            }
            break;
        }
    } else if input
        .get(cursor)
        .is_some_and(|unit| matches!(*unit, 0x40 | 0x2a | 0x23 | 0x3f | 0x24 | 0x21 | 0x30..=0x39 | 0x2d))
    {
        cursor += 1;
    } else {
        return None;
    }
    let cursor = line_continuation_end(input, cursor);
    (input.get(cursor) == Some(&u16::from(b'['))).then_some(cursor + 1)
}

pub(crate) fn parameter_prefix_end(input: &[u16], index: usize) -> usize {
    let mut cursor = line_continuation_end(input, index);
    if input.get(cursor) == Some(&u16::from(b'('))
        && let Some(close) =
            input.get(cursor + 1..).and_then(|tail| tail.iter().position(|unit| *unit == u16::from(b')')))
    {
        cursor += close + 2;
    }
    loop {
        if input.get(cursor).is_some_and(|unit| matches!(*unit, 0x21 | 0x23 | 0x5e | 0x3d | 0x2b | 0x7e)) {
            cursor += 1;
            continue;
        }
        let next = line_continuation_end(input, cursor);
        if next > cursor {
            cursor = next;
            continue;
        }
        break;
    }
    cursor
}

pub(crate) fn parameter_name_end(input: &[u16], index: usize) -> Option<usize> {
    let mut cursor = index;
    if input.get(cursor).is_some_and(|unit| matches!(*unit, 0x41..=0x5a | 0x61..=0x7a | 0x5f)) {
        cursor += 1;
        loop {
            if input.get(cursor).is_some_and(|unit| matches!(*unit, 0x41..=0x5a | 0x61..=0x7a | 0x30..=0x39 | 0x5f)) {
                cursor += 1;
                continue;
            }
            let next = line_continuation_end(input, cursor);
            if next > cursor {
                cursor = next;
                continue;
            }
            break;
        }
    } else if input.get(cursor).is_some_and(|unit| matches!(*unit, 0x30..=0x39)) {
        cursor += 1;
        loop {
            if input.get(cursor).is_some_and(|unit| matches!(*unit, 0x30..=0x39)) {
                cursor += 1;
                continue;
            }
            let next = line_continuation_end(input, cursor);
            if next > cursor {
                cursor = next;
                continue;
            }
            break;
        }
    } else if input.get(cursor).is_some_and(|unit| matches!(*unit, 0x40 | 0x2a | 0x23 | 0x3f | 0x24 | 0x21 | 0x2d)) {
        cursor += 1;
    } else {
        return None;
    }
    Some(line_continuation_end(input, cursor))
}

pub(crate) fn parameter_prompt_at(input: &[u16], index: usize) -> bool {
    if input.get(index) != Some(&u16::from(b'@')) {
        return false;
    }
    let cursor = line_continuation_end(input, index + 1);
    if !input.get(cursor).is_some_and(|unit| matches!(*unit, 0x45 | 0x50)) {
        return false;
    }
    let cursor = line_continuation_end(input, cursor + 1);
    input.get(cursor) == Some(&u16::from(b'}'))
}

pub(crate) fn indirect_at(input: &[u16], index: usize) -> bool {
    input.get(line_continuation_end(input, index)) == Some(&u16::from(b'!'))
}

pub(crate) fn is_js_space(unit: u16) -> bool {
    matches!(unit, 0x09..=0x0d | 0x20 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}

pub(crate) fn trim_units(input: &[u16], start: usize, end: usize) -> String {
    let end = end.min(input.len());
    let mut from = start;
    while from < end && is_js_space(input[from]) {
        from += 1;
    }
    let mut to = end;
    while to > from && is_js_space(input[to - 1]) {
        to -= 1;
    }
    units(input, from, to)
}

pub(crate) fn trim_js(text: &str) -> String {
    let input = encode(text);
    trim_units(&input, 0, input.len())
}

pub(crate) static BASH_REDIRECTS: &[&str] = &["&>>", "&>", "<<<", "<<-", "<<", "<>", "<&", ">&", ">|", ">>", ">", "<"];
pub(crate) static BASH_LIST_OPERATORS: &[&str] = &["&&", "||", "|&"];
pub(crate) static ZSH_LIST_OPERATORS: &[&str] = &["&&", "||", "|&", "&!", "&|"];
pub(crate) static BASH_CASE_OPERATORS: &[&str] = &[";;&", ";;", ";&", ";|", "&&", "||", "|&"];
pub(crate) static ZSH_CASE_OPERATORS: &[&str] = &[";;&", ";;", ";&", ";|", "&&", "||", "|&", "&!", "&|"];

pub(crate) static BASH_BINDING_BUILTINS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "declare",
        "typeset",
        "local",
        "export",
        "readonly",
        "private",
        "unset",
        "read",
        "getln",
        "vared",
        "getopts",
        "mapfile",
        "readarray",
        "compgen",
        "zparseopts",
        "zregexparse",
        "zformat",
        "zstyle",
        "zstat",
        "stat",
    ]
    .into_iter()
    .collect()
});
pub(crate) static BASH_ARITHMETIC_COMPARISONS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| ["-eq", "-ne", "-lt", "-le", "-gt", "-ge"].into_iter().collect());
pub(crate) static BASH_ASSIGNMENT_DECLARATIONS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| ["declare", "typeset", "export", "readonly", "local"].into_iter().collect());
pub(crate) static BASH_DECLARATIONS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| ["declare", "typeset", "export", "readonly", "local", "unset", "unsetenv"].into_iter().collect());
pub(crate) static BASH_PRECOMMANDS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| ["builtin", "command", "time", "coproc", "!"].into_iter().collect());
pub(crate) static ZSH_PRECOMMANDS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| ["noglob", "nocorrect", "exec", "-"].into_iter().collect());
pub(crate) static ZSH_ARITHMETIC_BUILTINS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    ["integer", "float", "exit", "return", "shift", "break", "continue", "bye", "logout"].into_iter().collect()
});
pub(crate) static BASH_NON_FUNCTION_KEYWORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    ["if", "for", "select", "case", "then", "elif", "else", "fi", "do", "done", "in", "esac"].into_iter().collect()
});

pub(crate) static COPROC_AHEAD_RE: LazyLock<Pattern> = LazyLock::new(|| {
    Pattern::new(
        r"coproc(?:[ \t]|\\\n)+(?:--(?:[ \t]|\\\n)+)?(?:[A-Za-z_][A-Za-z0-9_]*(?:[ \t]|\\\n)+)?(?=(?:!(?:[ \t]|\\\n)*)*(?:[{(]|\[\[(?:\\\n)*[ \t\n]|(?:if|while|until|for|select|case|repeat)\b))",
    )
});
pub(crate) static TIME_AHEAD_RE: LazyLock<Pattern> = LazyLock::new(|| {
    Pattern::new(
        r"time(?:[ \t]|\\\n)+(?:-p(?:[ \t]|\\\n)+)?(?:--(?:[ \t]|\\\n)+)?(?=(?:!(?:[ \t]|\\\n)*)*(?:[{(]|\[\[(?:\\\n)*[ \t\n]|(?:if|while|until|for|select|case|repeat)\b))",
    )
});
pub(crate) static FUNCTION_HEAD_RE: LazyLock<Pattern> = LazyLock::new(|| {
    Pattern::new(
        r"(function[ \t](?:[ \t]|\\\n)*)?([A-Za-z_][\w.:+@%-]*)?(?:[ \t]|\\\n)*(\([ \t]*\))?((?:[ \t\n]|\\\n|#[^\n]*\n)*)",
    )
});
pub(crate) static FUNCTION_BODY_RE: LazyLock<Pattern> = LazyLock::new(|| {
    Pattern::new(r"[{(]|\[\[(?=(?:\\\n)*[ \t\n])|(?:if|while|until|for|select|case|repeat)(?=(?:\\\n)*(?:[ \t\n(]|$))")
});
pub(crate) static DO_AHEAD_RE: LazyLock<Pattern> = LazyLock::new(|| {
    Pattern::new(r"(?:[ \t\n;]|\\\n|#[^\n]*(?:\n|$))*(?:do(?=(?:\\\n)*(?:[ \t\n;{(]|$))|\{(?=(?:\\\n)*[ \t\n]))")
});
pub(crate) static ZSH_SAFE_FLAGS_RE: LazyLock<Pattern> = LazyLock::new(|| {
    Pattern::new(r"\$\{\((?:[@AaBbCcDEfFikLMmNnOopPqStUuVvWwXZz0-]|q[+]|[js]([^({[<\\])(?:(?!\1)[^)])*\1)*\)")
});
pub(crate) static ZSH_QUOTED_FLAG_ARG_RE: LazyLock<Pattern> =
    LazyLock::new(|| Pattern::new(r"[js]([^({[<\\])(?:(?!\1)[^)])*\1"));
pub(crate) static ZSH_GLOB_QUALIFIER_RE: LazyLock<Pattern> = LazyLock::new(|| {
    Pattern::new(
        r"\((?:[/F.@=*rwxAIERWsStUG^MTNDn-]|[oO][nLlamcdN]|[amc][Mwhmsd]?[-+]?\d+|L[kKmMpPgGtT]?[-+]?\d+|[lY][-+]?\d+|\[[-+]?\d+(?:,[-+]?\d+)?\]|,)+\)(?=(?:\\\n)*(?:[ \t\n;&|<>)]|$))",
    )
});
pub(crate) static BASH_EXPANSION_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::new(r"\$(?:\\.|\\\n)*[({]|`"));
pub(crate) static BASH_ESCAPE_RE: LazyLock<Pattern> =
    LazyLock::new(|| Pattern::new(r"\\(?:x[\da-fA-F]{1,2}|[0-7]{1,3}|u[\da-fA-F]{1,4}|U[\da-fA-F]{1,8})"));
