use super::bash_grammar::scan_bash_command_start;
use super::bash_words::{
    BashEvaluation, HeredocDelimiter, SPAN_ASSIGNMENT_SUBSCRIPT, SPAN_HEREDOC, TextMode, bash_evaluates_expansion,
    bash_heredoc, bash_heredoc_delimiter, scan_bash_array_or_pattern, scan_bash_nested, scan_bash_span,
    scan_bash_subscript, scan_bash_unit,
};
use super::*;

pub(crate) static NULLCMD_EXACT_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::new(r"^(?:READ)?NULLCMD$"));
pub(crate) static OPTION_NAME_EXACT_RE: LazyLock<Pattern> =
    LazyLock::new(|| Pattern::new(r"^(?:(?:dis_)?(?:functions|[gs]?aliases)|options|commands)$"));
static OPTION_NAME_PREFIX_RE: LazyLock<Pattern> = LazyLock::new(|| {
    Pattern::new(r"^(?:-[A-Za-z]*v)?(?:(?:dis_)?(?:functions|[gs]?aliases)|options|commands)(?:\+?=|\[|$)")
});
static SET_ARITHMETIC_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::new(r"^[-+][A-Za-z0-9]*A"));
static DECLARED_ARRAY_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::new(r"\+?=(?:\(|[$`])"));
static PRINTF_V_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::new(r"^-v"));
static PRINT_V_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::new(r"^-[A-Za-z]*v"));
static WAIT_P_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::new(r"^-[nf]*p"));
static WAIT_P_EXACT_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::new(r"^-[nf]*p$"));
static SET_K_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::new(r"^-[A-Za-z0-9]*k"));
static SET_O_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::new(r"^[-+][A-Za-z0-9]*o"));
static SET_M_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::new(r"^-[A-Za-z0-9]*m"));
static ZSH_OPTION_RE: LazyLock<Pattern> =
    LazyLock::new(|| Pattern::new(r"^(?:no)?(?:globsubst|kshglob|promptsubst|promptvars|aliases)$"));
static ZSH_OPTION_PREFIX_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::with_flags(r"^[-+][A-Za-z0-9]*?o", "i"));
static COMPGEN_EXEC_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::new(r"[CF]"));
static FD_PREFIX_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::new(r"^(?:\d+|\{[A-Za-z_][A-Za-z0-9_]*\})$"));
static DUP_REDIRECT_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::new(r"^(?:\d+-?|-)$"));
static ASSIGNMENT_NAME_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::new(r"^[A-Za-z_][A-Za-z0-9_]*\+?$"));
static ASSIGNMENT_BASE_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::new(r"^[A-Za-z_][A-Za-z0-9_]*$"));
static EXEC_OPTION_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::new(r"^-[cl]*a$"));
static SLICE_SPACE_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::new(r"[$`]"));
static GLOB_SUFFIX_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::new(r"[?*+@!]$"));

pub(crate) struct BashHeredoc {
    pub delimiter: String,
    pub dash_delimiter: Option<String>,
    pub quoted: bool,
    pub tabs: bool,
    pub binding: bool,
    pub owned: bool,
    pub command: Option<usize>,
    pub start: Option<usize>,
}

impl BashHeredoc {
    fn from_delimiter(delimiter: HeredocDelimiter) -> Self {
        Self {
            delimiter: delimiter.delimiter,
            dash_delimiter: delimiter.dash_delimiter,
            quoted: delimiter.quoted,
            tabs: delimiter.tabs,
            binding: false,
            owned: false,
            command: None,
            start: None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum StructureKind {
    If,
    While,
    Until,
    For,
    Case,
    Repeat,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    Header,
    Condition,
    Pattern,
    Body,
    Do,
}

#[derive(Clone, Copy)]
pub(crate) struct Structure {
    pub(crate) kind: StructureKind,
    pub(crate) phase: Phase,
    pub(crate) count: usize,
    pub(crate) saw_else: bool,
    pub(crate) saw_in: bool,
    pub(crate) pattern_started: bool,
    pub(crate) parenthesized: bool,
}

impl Structure {
    pub(crate) fn new(kind: StructureKind, phase: Phase) -> Self {
        Self { kind, phase, count: 0, saw_else: false, saw_in: false, pattern_started: false, parenthesized: false }
    }
}

pub(crate) struct BashState<'a> {
    pub(crate) input: &'a [u16],
    pub(crate) context: &'a mut BashContext,
    pub(crate) commands: Vec<Command>,
    pub(crate) nested_commands: Vec<Command>,
    pub(crate) words: Vec<String>,
    pub(crate) raw_words: Vec<String>,
    pub(crate) heredocs: &'a mut Vec<BashHeredoc>,
    pub(crate) heredocs_shared: bool,
    pub(crate) structures: Vec<Structure>,
    pub(crate) word: String,
    pub(crate) literal: String,
    pub(crate) output: String,
    pub(crate) word_started: bool,
    pub(crate) word_start: usize,
    pub(crate) word_end: usize,
    pub(crate) command_start: Option<usize>,
    pub(crate) redirect_word_count: Option<usize>,
    pub(crate) redirect_prefix_end: Option<usize>,
    pub(crate) ampersand_redirect_words: Option<usize>,
    pub(crate) command_word_index: i64,
    pub(crate) assignment_word: bool,
    pub(crate) assignment_head_unsafe: bool,
    pub(crate) invalid: Option<OpaqueReason>,
    pub(crate) redirect_target: bool,
    pub(crate) dup_redirect: bool,
    pub(crate) here_string: bool,
    pub(crate) has_redirect: bool,
    pub(crate) has_file_redirect: bool,
    pub(crate) compound_end: bool,
    pub(crate) dangling: bool,
    pub(crate) outer: Option<&'a mut BashText>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn scan_bash(
    input: &[u16],
    start: usize,
    depth: usize,
    context: &mut BashContext,
    close: Option<Close>,
    heredocs: &mut Vec<BashHeredoc>,
    heredocs_shared: bool,
    outer: Option<&mut BashText>,
) -> BashResult {
    if depth > MAX_SUBSTITUTION_DEPTH || context.remaining < 0 {
        return BashResult::Opaque(OpaqueReason::InvalidStructure);
    }
    let mut state = BashState {
        input,
        context,
        commands: Vec::new(),
        nested_commands: Vec::new(),
        words: Vec::new(),
        raw_words: Vec::new(),
        heredocs,
        heredocs_shared,
        structures: Vec::new(),
        word: String::new(),
        literal: String::new(),
        output: String::new(),
        word_started: false,
        word_start: start,
        word_end: start,
        command_start: None,
        redirect_word_count: None,
        redirect_prefix_end: None,
        ampersand_redirect_words: None,
        command_word_index: -1,
        assignment_word: false,
        assignment_head_unsafe: false,
        invalid: None,
        redirect_target: false,
        dup_redirect: false,
        here_string: false,
        has_redirect: false,
        has_file_redirect: false,
        compound_end: false,
        dangling: false,
        outer,
    };
    let mut index = start;
    while index < input.len() {
        state.context.remaining -= 1;
        if state.context.remaining < 0 {
            return BashResult::Opaque(OpaqueReason::InvalidStructure);
        }
        let ch = input[index];
        if ch == b'\\'.into() && input.get(index + 1) == Some(&b'\n'.into()) {
            index += 2;
            continue;
        }
        if !state.word_started {
            state.word_start = index;
            if ch == b' '.into() || ch == b'\t'.into() {
                index += 1;
                continue;
            }
            if ch == b'}'.into()
                && state.words.is_empty()
                && !state.redirect_target
                && (close == Some(Close::Brace) || close == Some(Close::Nofork))
                && state.structures.is_empty()
                && (state.heredocs_shared || state.heredocs.is_empty())
                && (close == Some(Close::Nofork) || brace_close_ahead(input, index + 1))
            {
                return close_bash_list(&mut state, index);
            }
            if ch == b'}'.into() {
                return BashResult::Opaque(if state.structures.is_empty() {
                    OpaqueReason::InvalidStructure
                } else {
                    OpaqueReason::CompoundCommand
                });
            }
            match scan_bash_command_start(&mut state, index, depth) {
                Err(reason) => return BashResult::Opaque(reason),
                Ok(Some(step)) => {
                    index = step + 1;
                    continue;
                }
                Ok(None) => {}
            }
        }
        if ch == b'\\'.into() && index + 1 >= input.len() {
            return BashResult::Opaque(OpaqueReason::UnterminatedEscape);
        }
        if !state.assignment_word
            && (matches!(ch, 0x27 | 0x22 | 0x5c)
                || (ch == b'$'.into() && matches!(input.get(index + 1), Some(&0x27) | Some(&0x22))))
        {
            state.assignment_head_unsafe = true;
        }
        let in_case_pattern = state.structures.last().is_some_and(|item| item.phase == Phase::Pattern);
        let array = (ch == b'='.into() || ch == b'('.into())
            && state.assignment_word
            && state.word.len() > 1
            && state.word.ends_with('=')
            && !state.word.ends_with("==")
            && (state.command_word_index < 0
                || state
                    .raw_words
                    .get(state.command_word_index as usize)
                    .is_some_and(|word| BASH_ASSIGNMENT_DECLARATIONS.contains(word.as_str())));
        if ch == b'='.into() && (!state.word_started || array) && input.get(index + 1) == Some(&b'('.into()) {
            let end = match scan_bash_nested(
                input,
                &mut *state.context,
                &mut state.nested_commands,
                index + 2,
                depth,
                Close::Paren,
                None,
            ) {
                Ok(end) => end,
                Err(reason) => return BashResult::Opaque(reason),
            };
            state.word_started = true;
            state.word.push_str(&units(input, index, end + 1));
            index = end + 1;
            state.word_end = index;
            continue;
        }
        let allow_bracket = !state.assignment_word
            && state.command_word_index >= 0
            && !state.raw_words.first().is_some_and(|word| BASH_DECLARATIONS.contains(word.as_str()));
        let unit = if is_unit_start(ch) {
            let mut text = BashText {
                word: std::mem::take(&mut state.word),
                literal: std::mem::take(&mut state.literal),
                output: std::mem::take(&mut state.output),
            };
            let unit = scan_bash_unit(
                input,
                &mut *state.context,
                &mut state.nested_commands,
                index,
                depth,
                TextMode::Word,
                allow_bracket,
                Some(&mut text),
            );
            state.word = text.word;
            state.literal = text.literal;
            state.output = text.output;
            unit
        } else {
            Ok(None)
        };
        match unit {
            Err(reason) => return BashResult::Opaque(reason),
            Ok(Some(end)) => {
                state.word_started = true;
                index = end + 1;
                state.word_end = index;
                continue;
            }
            Ok(None) => {}
        }
        if ch == b'['.into()
            && !in_case_pattern
            && !state.assignment_word
            && !state.assignment_head_unsafe
            && state.command_word_index < 0
            && ASSIGNMENT_BASE_RE.is_match_str(&state.word)
        {
            let mut subscript = Vec::new();
            let end = match scan_bash_subscript(
                input,
                &mut *state.context,
                &mut subscript,
                index + 1,
                depth,
                &SPAN_ASSIGNMENT_SUBSCRIPT,
                None,
            ) {
                Ok(end) => end,
                Err(reason) => return BashResult::Opaque(reason),
            };
            let assign = match bash_operator(input, end + 1, &["+=", "="]) {
                Ok(assign) => assign,
                Err(reason) => return BashResult::Opaque(reason),
            };
            if assign.is_some() {
                if state.context.dialect == Dialect::Posix && SLICE_SPACE_RE.is_match(&input[index + 1..end]) {
                    return BashResult::Opaque(OpaqueReason::InvalidStructure);
                }
                state.nested_commands.append(&mut subscript);
                state.assignment_word = true;
                state.word.push_str(&units(input, index, end + 1));
                index = end + 1;
                state.word_end = index;
                continue;
            }
        }
        if ch == b'('.into()
            && (array
                || ((in_case_pattern || state.context.dialect == Dialect::Bash)
                    && GLOB_SUFFIX_RE.is_match_str(&state.word)))
        {
            let mode = if array { ArrayMode::Array } else { ArrayMode::Pattern };
            let end = match scan_bash_array_or_pattern(
                input,
                &mut *state.context,
                &mut state.nested_commands,
                index,
                depth + 1,
                mode,
            ) {
                Ok(end) => end,
                Err(reason) => return BashResult::Opaque(reason),
            };
            if bash_crosses_heredoc(&state, index, end) {
                return BashResult::Opaque(OpaqueReason::Heredoc);
            }
            if array
                && end + 1 < input.len()
                && !matches!(input[end + 1], 0x20 | 0x09 | 0x0a | 0x3b | 0x26 | 0x7c | 0x3c | 0x3e | 0x29)
            {
                return BashResult::Opaque(OpaqueReason::InvalidStructure);
            }
            state.word_started = true;
            state.word.push_str(&units(input, index, end + 1));
            index = end + 1;
            state.word_end = index;
            continue;
        }
        if ch == b'#'.into() && !state.word_started {
            match index_of(input, index, "\n") {
                Some(newline) => {
                    index = newline;
                    continue;
                }
                None => break,
            }
        }
        if matches!(ch, 0x3c | 0x3e | 0x26 | 0x7c | 0x3b | 0x0a) {
            match scan_bash_operator_or_separator(&mut state, index, depth) {
                Err(reason) => return BashResult::Opaque(reason),
                Ok(step) => {
                    index = step + 1;
                    continue;
                }
            }
        }
        if in_case_pattern && ch == b')'.into() {
            finish_bash_word(&mut state);
            continue;
        }
        if ch == b')'.into() {
            if close != Some(Close::Paren)
                || !state.structures.is_empty()
                || (!state.heredocs_shared && !state.heredocs.is_empty())
            {
                return BashResult::Opaque(OpaqueReason::CompoundCommand);
            }
            return close_bash_list(&mut state, index);
        }
        if ch == b'('.into()
            && state.context.dialect == Dialect::Zsh
            && state.word_started
            && !in_case_pattern
            && ZSH_GLOB_QUALIFIER_RE.test_at(input, index)
        {
            let end = ZSH_GLOB_QUALIFIER_RE.end_at(input, index).unwrap_or(index) - 1;
            bash_append(&mut state, &units(input, index, end + 1));
            index = end + 1;
            state.word_end = index;
            continue;
        }
        if ch == b'('.into() {
            return BashResult::Opaque(if state.word_started {
                OpaqueReason::DynamicExecution
            } else {
                OpaqueReason::CompoundCommand
            });
        }
        if is_js_space(ch) && !matches!(ch, 0x20 | 0x09 | 0x0a) {
            return BashResult::Opaque(OpaqueReason::InvalidStructure);
        }
        if ch == b' '.into() || ch == b'\t'.into() {
            finish_bash_word(&mut state);
            index += 1;
            continue;
        }
        state.word_started = true;
        if ch == b'='.into()
            && !state.assignment_word
            && !state.assignment_head_unsafe
            && ASSIGNMENT_NAME_RE.is_match_str(&state.word)
        {
            state.assignment_word = true;
        }
        push_unit(&mut state.word, ch);
        push_unit(&mut state.literal, ch);
        state.word_end = index + 1;
        index += 1;
    }

    if close.is_some() {
        return BashResult::Opaque(if close == Some(Close::Paren) {
            OpaqueReason::CommandSubstitution
        } else {
            OpaqueReason::InvalidStructure
        });
    }
    if !state.heredocs.is_empty() {
        return BashResult::Opaque(OpaqueReason::Heredoc);
    }
    close_bash_list(&mut state, input.len())
}

fn is_unit_start(ch: u16) -> bool {
    matches!(ch, 0x5c | 0x24 | 0x27 | 0x22 | 0x60 | 0x3c | 0x3e)
}

pub(crate) fn bash_statement(state: &mut BashState) {
    if let Some(structure) = state.structures.last_mut() {
        structure.count += 1;
    }
}

pub(crate) fn bash_in_header(state: &BashState) -> bool {
    state
        .structures
        .last()
        .is_some_and(|structure| matches!(structure.phase, Phase::Header | Phase::Pattern | Phase::Do))
}

pub(crate) fn at_command_start(state: &BashState) -> bool {
    state.words.is_empty() && !state.has_redirect && !state.compound_end && !bash_in_header(state)
}

fn pending(state: &BashState) -> bool {
    state.word_started || !state.words.is_empty() || state.has_redirect || state.compound_end
}

pub(crate) fn end_bash_list(state: &mut BashState) -> bool {
    if pending(state) {
        finish_bash_command(state, false, true);
    }
    !state.dangling
}

fn close_bash_list(state: &mut BashState, end: usize) -> BashResult {
    if !end_bash_list(state) {
        return BashResult::Opaque(OpaqueReason::InvalidStructure);
    }
    if let Some(reason) = state.invalid {
        return BashResult::Opaque(reason);
    }
    if !state.structures.is_empty() {
        return BashResult::Opaque(OpaqueReason::CompoundCommand);
    }
    BashResult::Scanned { commands: std::mem::take(&mut state.commands), end }
}

pub(crate) fn finish_bash_word(state: &mut BashState) {
    if !state.word_started {
        return;
    }
    if state.word.contains("PS4") {
        state.context.eval_sink = true;
    }
    let evaluation = bash_word_evaluation(state);
    if state.context.dialect != Dialect::Bash
        && (state.word.contains("NULLCMD")
            || ((state.assignment_word || evaluation == Some(BashEvaluation::Binding))
                && OPTION_NAME_PREFIX_RE.is_match_str(&state.word)))
    {
        state.invalid.get_or_insert(OpaqueReason::DynamicExecution);
    }
    if let Some(evaluation) = evaluation {
        let text = BashText { word: state.word.clone(), literal: state.literal.clone(), output: state.output.clone() };
        if bash_evaluates_expansion(&text, evaluation, state.context) {
            state.invalid.get_or_insert(OpaqueReason::DynamicExecution);
        }
    }
    if state.word != state.literal && state.command_word_index >= 0 {
        let builtin = state.words.get(bash_builtin_index(state) as usize).map(String::as_str);
        if matches!(builtin, Some("declare" | "typeset" | "local")) {
            state.context.eval_sink = true;
        }
    }
    if let Some(outer) = state.outer.as_deref_mut() {
        outer.output.push(' ');
        outer.output.push_str(&state.literal);
        outer.output.push(' ');
        outer.output.push_str(&state.output);
    }
    let repeat_header = state
        .structures
        .last()
        .is_some_and(|structure| structure.kind == StructureKind::Repeat && structure.phase == Phase::Header);
    if !state.redirect_target
        && repeat_header
        && let Some(structure) = state.structures.last_mut()
    {
        structure.phase = Phase::Do;
    }
    if !state.redirect_target && !repeat_header {
        if !state.assignment_word && state.command_word_index < 0 {
            state.command_word_index = state.words.len() as i64;
        }
        if state.command_start.is_none() {
            state.command_start = Some(state.word_start);
        }
        state.words.push(state.word.clone());
        state.raw_words.push(units(state.input, state.word_start, state.word_end));
    }
    if state.redirect_target {
        if !state.dup_redirect || !DUP_REDIRECT_RE.is_match(&state.input[state.word_start..state.word_end]) {
            state.has_file_redirect = true;
        }
        if state.redirect_prefix_end.is_none() {
            state.redirect_prefix_end = Some(state.word_end);
        }
    }
    state.redirect_target = false;
    state.dup_redirect = false;
    state.here_string = false;
    state.word.clear();
    state.literal.clear();
    state.output.clear();
    state.word_started = false;
    state.assignment_word = false;
    state.assignment_head_unsafe = false;
}

fn bash_word_evaluation(state: &BashState) -> Option<BashEvaluation> {
    if state.redirect_target {
        return state.here_string.then_some(BashEvaluation::Deferred);
    }
    let structure = state.structures.last().copied();
    if structure.is_some_and(|item| item.kind == StructureKind::Repeat && item.phase == Phase::Header) {
        return Some(BashEvaluation::Arithmetic);
    }
    if structure.is_some_and(|item| item.kind == StructureKind::For && item.phase == Phase::Header && item.saw_in) {
        return Some(BashEvaluation::Deferred);
    }
    if state.command_word_index < 0 {
        return state.assignment_word.then_some(BashEvaluation::Deferred);
    }
    let name = bash_builtin_index(state) as usize;
    let command = state.words.get(name).map(String::as_str)?;
    if command == "let" || (state.context.dialect != Dialect::Bash && ZSH_ARITHMETIC_BUILTINS.contains(command)) {
        return Some(BashEvaluation::Arithmetic);
    }
    if command == "set" {
        return if state.words[name + 1..].iter().any(|word| SET_ARITHMETIC_RE.is_match_str(word)) {
            Some(BashEvaluation::Binding)
        } else {
            Some(BashEvaluation::Deferred)
        };
    }
    if BASH_BINDING_BUILTINS.contains(command) {
        let options = &state.words[name + 1..];
        if options
            .iter()
            .any(|word| word.starts_with('-') && word[1..].chars().any(|ch| matches!(ch, 'E' | 'F' | 'i' | 'n')))
        {
            return Some(BashEvaluation::Arithmetic);
        }
        if state.assignment_word
            && BASH_ASSIGNMENT_DECLARATIONS.contains(command)
            && options.iter().all(|word| !word.starts_with('-'))
            && (command == "export"
                || command == "readonly"
                || state.context.dialect == Dialect::Zsh
                || !DECLARED_ARRAY_RE.is_match_str(&state.word))
        {
            return Some(BashEvaluation::Deferred);
        }
        return Some(BashEvaluation::Binding);
    }
    if command == "printf" || command == "print" {
        let mut options = state.words[name + 1..].to_vec();
        options.push(state.word.clone());
        let flag = if command == "print" { &*PRINT_V_RE } else { &*PRINTF_V_RE };
        return options.iter().any(|word| flag.is_match_str(word)).then_some(BashEvaluation::Binding);
    }
    if command == "wait" {
        let last = state.words.last().map(String::as_str).unwrap_or("");
        return (WAIT_P_RE.is_match_str(&state.word) || WAIT_P_EXACT_RE.is_match_str(last))
            .then_some(BashEvaluation::Binding);
    }
    if command == "test" || command == "[" {
        if state.words.last().map(String::as_str) == Some("-v") {
            return Some(BashEvaluation::Binding);
        }
        if state.context.dialect != Dialect::Bash && state.words.last().map(String::as_str) == Some("-t") {
            return Some(BashEvaluation::Arithmetic);
        }
    }
    None
}

fn bash_builtin_index(state: &BashState) -> i64 {
    let mut name = state.command_word_index;
    loop {
        let word = state.words.get(name as usize).map(String::as_str).unwrap_or("");
        if !(BASH_PRECOMMANDS.contains(word)
            || (state.context.dialect != Dialect::Bash && ZSH_PRECOMMANDS.contains(word)))
        {
            break;
        }
        let precommand = state.words[name as usize].clone();
        name += 1;
        while precommand != "-" && state.words.get(name as usize).is_some_and(|word| word.starts_with('-')) {
            if precommand == "exec" && EXEC_OPTION_RE.is_match_str(&state.words[name as usize]) {
                name += 1;
            }
            name += 1;
        }
    }
    name
}

pub(crate) fn finish_bash_command(state: &mut BashState, boundary: bool, newline: bool) {
    finish_bash_word(state);
    if state.redirect_target {
        state.invalid.get_or_insert(OpaqueReason::InvalidRedirect);
    }
    state.redirect_target = false;
    if state.compound_end && !state.words.is_empty() {
        state.invalid.get_or_insert(OpaqueReason::InvalidStructure);
    }
    let name = state.command_word_index;
    let in_header = bash_in_header(state);
    if name >= 0 && state.words.get(name as usize).is_none_or(|word| word.is_empty()) {
        state.invalid.get_or_insert(OpaqueReason::InvalidStructure);
    }
    let mut owner: Option<usize> = None;
    if name >= 0 && !in_header {
        let command_start = state.command_start.unwrap_or(state.word_end);
        let resource = trim_units(state.input, command_start, state.word_end);
        let words = state.words[name as usize..].to_vec();
        let raw_words = state.raw_words[name as usize..].to_vec();
        let declaration = name == 0
            && !state.has_redirect
            && state.raw_words.first().is_some_and(|word| BASH_DECLARATIONS.contains(word.as_str()))
            && state.raw_words.first().is_some_and(|word| resource.starts_with(word));
        let redirect_word_count = state
            .redirect_word_count
            .and_then(|count| (count < state.words.len()).then_some(count.wrapping_sub(name as usize)));
        state.commands.push(Command {
            resource,
            words,
            raw_words,
            word_ends: None,
            statement_head: None,
            declaration: declaration.then_some(true),
            redirect_word_count,
            redirect_prefix_end: None,
        });
        owner = Some(state.commands.len() - 1);
        let builtin = bash_builtin_index(state) as usize;
        let builtin_name = state.words.get(builtin).map(String::as_str);
        let builtin_args = state.words.get(builtin + 1..).unwrap_or(&[]);
        if builtin_name == Some("eval") {
            state.context.eval_sink = true;
        }
        if builtin_name == Some("set")
            && builtin_args.iter().any(|word| SET_K_RE.is_match_str(word) || word == "keyword")
        {
            state.invalid.get_or_insert(OpaqueReason::DynamicExecution);
        }
        if builtin_name == Some("alias") && builtin_args.iter().any(|word| word.contains('=')) {
            state.invalid.get_or_insert(OpaqueReason::DynamicExecution);
        }
        if state.context.dialect != Dialect::Bash
            && (builtin_name == Some("emulate")
                || (matches!(builtin_name, Some("setopt" | "unsetopt"))
                    || (builtin_name == Some("set") && builtin_args.iter().any(|word| SET_O_RE.is_match_str(word))))
                    && builtin_args.iter().any(|word| {
                        SET_M_RE.is_match_str(word)
                            || ZSH_OPTION_RE.is_match_str(
                                &ZSH_OPTION_PREFIX_RE
                                    .replace_all(&encode(word), "")
                                    .to_lowercase()
                                    .replace(['_', '-'], ""),
                            )
                    }))
        {
            state.invalid.get_or_insert(OpaqueReason::DynamicExecution);
        }
        if state.context.dialect != Dialect::Zsh
            && builtin_name == Some("compgen")
            && builtin_args.iter().any(|word| word.starts_with('-') && COMPGEN_EXEC_RE.is_match_str(word))
        {
            state.invalid.get_or_insert(OpaqueReason::DynamicExecution);
        }
        if builtin_name.is_some_and(|word| BASH_BINDING_BUILTINS.contains(word)) {
            for heredoc in state.heredocs.iter_mut() {
                heredoc.binding = true;
            }
        }
    } else if name < 0
        && !in_header
        && state.command_start.is_some()
        && (state.has_file_redirect || (state.has_redirect && !state.compound_end))
    {
        let command_start = state.command_start.unwrap_or(0);
        let raw_span = units(state.input, command_start, state.word_end);
        let raw = encode(&raw_span);
        let leading = raw.iter().take_while(|unit| matches!(unit, 0x20 | 0x09 | 0x0a)).count();
        let resource = trim_units(&raw, leading, raw.len());
        if !resource.is_empty() {
            let resource_units = encode(&resource);
            let prefix_end = match state.redirect_prefix_end {
                Some(prefix) => {
                    let extent = prefix as i64 - command_start as i64 - leading as i64;
                    1.max(resource_units.len().min(extent.max(0) as usize))
                }
                None => resource_units.len(),
            };
            let prefix_token = units(&resource_units, 0, prefix_end);
            state.commands.push(Command {
                resource,
                words: vec![prefix_token.clone()],
                raw_words: vec![prefix_token],
                word_ends: None,
                statement_head: None,
                declaration: None,
                redirect_word_count: None,
                redirect_prefix_end: Some(prefix_end),
            });
            owner = Some(state.commands.len() - 1);
        }
    }
    for heredoc in state.heredocs.iter_mut() {
        if owner.is_some() && newline && !heredoc.owned {
            heredoc.command = owner;
            heredoc.start = state.command_start;
        }
        heredoc.owned = true;
    }
    if boundary && state.words.is_empty() && !state.has_redirect && !state.compound_end && !in_header {
        state.invalid.get_or_insert(OpaqueReason::InvalidStructure);
    }
    if state.words.len() > state.ampersand_redirect_words.unwrap_or(usize::MAX) {
        state.invalid.get_or_insert(OpaqueReason::InvalidRedirect);
    }
    state.commands.append(&mut state.nested_commands);
    if !in_header && (!state.words.is_empty() || state.has_redirect) {
        bash_statement(state);
    }
    state.words.clear();
    state.raw_words.clear();
    state.command_word_index = -1;
    state.command_start = None;
    state.dup_redirect = false;
    state.has_redirect = false;
    state.has_file_redirect = false;
    state.redirect_word_count = None;
    state.redirect_prefix_end = None;
    state.ampersand_redirect_words = None;
    state.compound_end = false;
    state.dangling = false;
}

fn scan_bash_operator_or_separator(state: &mut BashState, index: usize, depth: usize) -> Result<usize, OpaqueReason> {
    let input = state.input;
    let ch = input[index];
    let redirect = if matches!(ch, 0x3c | 0x3e | 0x26) { bash_operator(input, index, BASH_REDIRECTS)? } else { None };
    if let Some(redirect) = redirect {
        if state
            .structures
            .last()
            .is_some_and(|structure| structure.kind == StructureKind::Case && structure.phase != Phase::Body)
        {
            return Err(OpaqueReason::InvalidRedirect);
        }
        state.has_redirect = true;
        if state.command_start.is_none() {
            state.command_start = Some(state.word_start);
        }
        if state.redirect_target {
            state.invalid.get_or_insert(OpaqueReason::InvalidRedirect);
        }
        let fd_prefix = state.word_started && !state.assignment_head_unsafe && FD_PREFIX_RE.is_match_str(&state.word);
        if fd_prefix {
            state.word.clear();
            state.literal.clear();
            state.output.clear();
            state.word_started = false;
        }
        if !fd_prefix {
            finish_bash_word(state);
        }
        if redirect.starts_with('&')
            && state.context.dialect == Dialect::Posix
            && state.ampersand_redirect_words.is_none()
        {
            state.ampersand_redirect_words = Some(state.words.len());
        }
        if state.redirect_word_count.is_none() && state.command_word_index >= 0 {
            state.redirect_word_count = Some(state.words.len());
        }
        if redirect == "<<" || redirect == "<<-" {
            let delimiter =
                bash_heredoc_delimiter(input, index + redirect.len(), redirect == "<<-", state.context.dialect);
            let Some(delimiter) = delimiter else {
                return Err(OpaqueReason::InvalidRedirect);
            };
            state.has_file_redirect = true;
            if state.redirect_prefix_end.is_none() {
                state.redirect_prefix_end =
                    Some(if input.get(index + redirect.len()).is_some_and(|unit| matches!(unit, 0x20 | 0x09)) {
                        index + redirect.len()
                    } else {
                        delimiter.end + 1
                    });
            }
            let end = delimiter.end;
            state.heredocs.push(BashHeredoc::from_delimiter(delimiter));
            state.word_end = end + 1;
            state.redirect_target = false;
            state.dup_redirect = false;
            return Ok(end);
        }
        if input.get(index + redirect.len()).is_some_and(|unit| matches!(unit, 0x20 | 0x09 | 0x0a))
            && state.redirect_prefix_end.is_none()
        {
            state.redirect_prefix_end = Some(index + redirect.len());
        }
        state.redirect_target = true;
        state.dup_redirect = redirect == "<&" || redirect == ">&";
        state.here_string = redirect == "<<<";
        return Ok(index + redirect.len() - 1);
    }
    if state.structures.last().is_some_and(|structure| structure.phase == Phase::Pattern) && ch == 0x7c {
        finish_bash_word(state);
        return Ok(index.saturating_sub(1));
    }
    let structure = state.structures.last().copied();
    let case_body = structure.is_some_and(|item| item.kind == StructureKind::Case && item.phase == Phase::Body);
    let operators = match (state.context.dialect, case_body) {
        (Dialect::Zsh, true) => ZSH_CASE_OPERATORS,
        (Dialect::Zsh, false) => ZSH_LIST_OPERATORS,
        (_, true) => BASH_CASE_OPERATORS,
        (_, false) => BASH_LIST_OPERATORS,
    };
    let operator = bash_operator(input, index, operators)?;
    let separator = operator.unwrap_or_else(|| {
        char::from_u32(u32::from(ch))
            .map(|unit| match unit {
                '<' => "<",
                '>' => ">",
                '&' => "&",
                '|' => "|",
                ';' => ";",
                '\n' => "\n",
                _ => "",
            })
            .unwrap_or("")
    });
    if case_body && separator.starts_with(';') && separator.len() > 1 {
        if !end_bash_list(state) {
            return Err(OpaqueReason::InvalidStructure);
        }
        if let Some(structure) = state.structures.last_mut() {
            structure.phase = Phase::Pattern;
            structure.pattern_started = false;
        }
        return Ok(index + separator.len() - 1);
    }
    if let Some(structure) = state.structures.last()
        && structure.kind == StructureKind::Case
        && matches!(structure.phase, Phase::Header | Phase::Pattern)
    {
        if separator != "\n" {
            return Err(OpaqueReason::CompoundCommand);
        }
        if structure.phase == Phase::Pattern && (state.word_started || !state.words.is_empty()) {
            return Err(OpaqueReason::CompoundCommand);
        }
        finish_bash_word(state);
        return read_bash_heredocs(state, index, depth);
    }
    if let Some(structure) = state.structures.last()
        && matches!(structure.kind, StructureKind::For | StructureKind::Repeat)
        && matches!(structure.phase, Phase::Header | Phase::Do)
    {
        if separator != ";" && separator != "\n" {
            return Err(OpaqueReason::CompoundCommand);
        }
        if structure.phase == Phase::Header && !structure.saw_in && !state.word_started && state.words.is_empty() {
            return Err(OpaqueReason::CompoundCommand);
        }
    }
    if separator == "\n" && !pending(state) {
        return read_bash_heredocs(state, index, depth);
    }
    if separator == ";" && state.context.dialect == Dialect::Zsh && !pending(state) && !state.dangling {
        return Ok(index);
    }
    finish_bash_command(state, true, separator == "\n");
    if let Some(structure) = state.structures.last_mut()
        && structure.kind == StructureKind::For
        && structure.phase == Phase::Header
    {
        structure.phase = Phase::Do;
    }
    state.dangling = !(separator == ";" || separator == "\n" || (separator.starts_with('&') && separator != "&&"));
    if separator == "\n" && !state.heredocs.is_empty() {
        return Ok(index.saturating_sub(1));
    }
    Ok(index + separator.len() - 1)
}

pub(crate) fn bash_command_key(command: &Command) -> (String, Vec<String>) {
    (command.resource.clone(), command.words.clone())
}

pub(crate) fn read_bash_heredocs(state: &mut BashState, index: usize, depth: usize) -> Result<usize, OpaqueReason> {
    let mut end = index;
    let pending = std::mem::take(state.heredocs);
    for heredoc in pending {
        let Some(body) = bash_heredoc(state.input, end + 1, &heredoc) else {
            return Err(OpaqueReason::Heredoc);
        };
        if let Some(owner) = heredoc.command {
            let start = heredoc.start.unwrap_or(end + 1);
            state.commands[owner].resource = trim_units(state.input, start, body.end);
        }
        let mut text = if heredoc.quoted {
            BashText { word: body.source.clone(), literal: body.source.clone(), output: String::new() }
        } else {
            BashText::default()
        };
        if !heredoc.quoted {
            let source = encode(&body.source);
            scan_bash_span(
                &source,
                &mut *state.context,
                &mut state.commands,
                0,
                depth,
                &SPAN_HEREDOC,
                true,
                Some(&mut text),
            )?;
        }
        if heredoc.binding {
            bash_evaluates_expansion(&text, BashEvaluation::Deferred, state.context);
        }
        end = body.end;
    }
    Ok(end)
}

pub(crate) fn bash_crosses_heredoc(state: &BashState, start: usize, end: usize) -> bool {
    !state.heredocs.is_empty() && index_of(state.input, start, "\n").is_some_and(|at| at < end)
}

pub(crate) fn bash_operator<'a>(
    input: &[u16],
    index: usize,
    operators: &[&'a str],
) -> Result<Option<&'a str>, OpaqueReason> {
    let mut text = String::new();
    let mut cursor = index;
    while unit_len(&text) < 3 && cursor < input.len() {
        if starts_with(input, cursor, "\\\n") {
            cursor += 1;
        } else {
            push_unit(&mut text, input[cursor]);
        }
        cursor += 1;
    }
    let operator = operators.iter().find(|candidate| text.starts_with(**candidate));
    if let Some(operator) = operator
        && !starts_with(input, index, operator)
    {
        return Err(OpaqueReason::InvalidStructure);
    }
    Ok(operator.copied())
}

pub(crate) fn bash_append(state: &mut BashState, value: &str) {
    state.word.push_str(value);
    state.literal.push_str(value);
}
