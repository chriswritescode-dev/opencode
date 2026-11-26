use super::bash::{BashHeredoc, NULLCMD_EXACT_RE, OPTION_NAME_EXACT_RE, bash_operator, scan_bash};
use super::*;

static DISALLOWED_ARITHMETIC_QUOTE_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::new(r#"[()[\];"}]"#));
static ANSI_QUOTE_UNSAFE_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::new("[#\"\\n]|\\\\'"));
static PREV_DIGIT_OR_DOLLAR_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::new(r"[\d$]"));

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum BashEvaluation {
    Arithmetic,
    Binding,
    Deferred,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextMode {
    Word,
    Quoted,
    Arithmetic,
}

pub(crate) struct BashSpan {
    pub open: Option<u8>,
    pub close: Option<u8>,
    pub reject: &'static str,
    pub mode: TextMode,
}

pub(crate) static SPAN_DOUBLE: BashSpan =
    BashSpan { open: None, close: Some(b'"'), reject: "", mode: TextMode::Quoted };
pub(crate) static SPAN_HEREDOC: BashSpan = BashSpan { open: None, close: None, reject: "", mode: TextMode::Quoted };
pub(crate) static SPAN_ARITHMETIC: BashSpan =
    BashSpan { open: Some(b'('), close: Some(b')'), reject: ";#}", mode: TextMode::Arithmetic };
pub(crate) static SPAN_FOR_ARITHMETIC: BashSpan =
    BashSpan { open: Some(b'('), close: Some(b')'), reject: "#}", mode: TextMode::Arithmetic };
pub(crate) static SPAN_BRACKET_ARITHMETIC: BashSpan =
    BashSpan { open: None, close: Some(b']'), reject: ";|&<>()[\n'\"\\#", mode: TextMode::Arithmetic };
pub(crate) static SPAN_SUBSCRIPT: BashSpan =
    BashSpan { open: Some(b'['), close: Some(b']'), reject: "\"}", mode: TextMode::Arithmetic };
pub(crate) static SPAN_QUOTED_SUBSCRIPT: BashSpan =
    BashSpan { open: Some(b'['), close: Some(b']'), reject: "\"", mode: TextMode::Quoted };
pub(crate) static SPAN_ASSIGNMENT_SUBSCRIPT: BashSpan =
    BashSpan { open: Some(b'['), close: Some(b']'), reject: " \t\n\r\x0b\x0c;&|<>()", mode: TextMode::Arithmetic };
pub(crate) static SPAN_ARITHMETIC_QUOTE: BashSpan =
    BashSpan { open: None, close: Some(b'\''), reject: "()[];\"}", mode: TextMode::Quoted };
pub(crate) static SPAN_ARITHMETIC_DOUBLE: BashSpan =
    BashSpan { open: None, close: Some(b'"'), reject: "()[];}", mode: TextMode::Quoted };
pub(crate) static SPAN_PARAMETER_QUOTE: BashSpan =
    BashSpan { open: None, close: Some(b'\''), reject: "\"}[]", mode: TextMode::Quoted };

pub(crate) struct AnsiQuote {
    pub value: String,
    pub end: usize,
}

pub(crate) struct HeredocDelimiter {
    pub delimiter: String,
    pub dash_delimiter: Option<String>,
    pub quoted: bool,
    pub tabs: bool,
    pub end: usize,
}

pub(crate) struct HeredocBody {
    pub source: String,
    pub end: usize,
}

fn append_text(text: &mut BashText, value: &str) {
    text.word.push_str(value);
    text.literal.push_str(value);
}

fn append_text_unit(text: &mut BashText, unit: u16) {
    push_unit(&mut text.word, unit);
    push_unit(&mut text.literal, unit);
}

fn zsh_evaluates(input: &[u16], index: usize) -> bool {
    let mut at = index + 1;
    if starts_with(input, at, "{\\\n") {
        return true;
    }
    if input.get(at) == Some(&u16::from(b'{')) && input.get(at + 1) == Some(&u16::from(b'(')) {
        if !ZSH_SAFE_FLAGS_RE.test_at(input, index) {
            return true;
        }
        at = ZSH_SAFE_FLAGS_RE.end_at(input, index).unwrap_or(at);
    }
    if input.get(at) == Some(&u16::from(b'{')) {
        at += 1;
    }
    zsh_glob_modifier_at(input, at)
}

pub(crate) fn bash_evaluates_expansion(text: &BashText, evaluation: BashEvaluation, context: &mut BashContext) -> bool {
    let combined = format!("{} {}", text.literal, text.output);
    let combined_units = encode(&combined);
    let has_expansion = BASH_EXPANSION_RE.is_match(&combined_units);
    let has_escape = BASH_ESCAPE_RE.is_match(&combined_units);
    if has_expansion || has_escape || text.literal.contains(['$', '`']) {
        context.bound_dollar = true;
    }
    if has_expansion || has_escape || text.literal.contains(['(', '{', '`']) {
        context.bound_open = true;
    }
    if has_expansion
        || has_escape
        || !trim_js(&text.output).is_empty()
        || combined.contains('[')
        || combined_units.contains(&u16::from(b'['))
    {
        context.bound_bracket = true;
    }
    if evaluation == BashEvaluation::Deferred {
        return false;
    }
    if text.word.contains('[') || combined_units.contains(&u16::from(b'[')) {
        context.sink_bracket = true;
    }
    let variable_sink = match evaluation {
        BashEvaluation::Arithmetic => ARITHMETIC_VARIABLE_RE.is_match_str(&text.word),
        _ => text.word.contains('$') || combined_units.contains(&u16::from(b'[')),
    };
    if variable_sink {
        context.variable_sink = true;
    }
    if evaluation == BashEvaluation::Arithmetic {
        return has_expansion;
    }
    combined_units.contains(&u16::from(b'[')) && has_expansion
}

static ARITHMETIC_VARIABLE_RE: LazyLock<Pattern> =
    LazyLock::new(|| Pattern::new(r"[A-Za-z_]|(?:\$(?![?#!-]|\$(?!\[)))"));

pub(crate) fn scan_bash_subscript(
    input: &[u16],
    context: &mut BashContext,
    commands: &mut Vec<Command>,
    start: usize,
    depth: usize,
    span: &BashSpan,
    text: Option<&mut BashText>,
) -> Result<usize, OpaqueReason> {
    let mut local = BashText::default();
    let target = text.unwrap_or(&mut local);
    let end = scan_bash_span(input, context, commands, start, depth, span, false, Some(target))?;
    context.sink_bracket = true;
    if bash_evaluates_expansion(target, BashEvaluation::Arithmetic, context) {
        return Err(OpaqueReason::DynamicExecution);
    }
    Ok(end)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn scan_bash_span(
    input: &[u16],
    context: &mut BashContext,
    commands: &mut Vec<Command>,
    start: usize,
    depth: usize,
    span: &BashSpan,
    allow_bracket: bool,
    mut text: Option<&mut BashText>,
) -> Result<usize, OpaqueReason> {
    let mut nesting: usize = 0;
    let mut index = start;
    while index < input.len() {
        context.remaining -= 1;
        if context.remaining < 0 {
            return Err(OpaqueReason::InvalidStructure);
        }
        let ch = input[index];
        if Some(ch) == span.close.map(u16::from) && nesting == 0 {
            return Ok(index);
        }
        let rejected = span.reject.contains(char::from_u32(u32::from(ch)).unwrap_or('\0'))
            && !(ch == u16::from(b'#')
                && input.get(index.wrapping_sub(1)).is_some_and(|unit| PREV_DIGIT_OR_DOLLAR_RE.is_match(&[*unit])));
        if rejected {
            return Err(OpaqueReason::InvalidStructure);
        }
        if Some(ch) == span.open.map(u16::from) {
            nesting += 1;
            if nesting + depth > MAX_SUBSTITUTION_DEPTH {
                return Err(OpaqueReason::InvalidStructure);
            }
        }
        if Some(ch) == span.close.map(u16::from) {
            nesting -= 1;
        }
        let unit = if Some(ch) == span.open.map(u16::from) || Some(ch) == span.close.map(u16::from) {
            Ok(None)
        } else {
            scan_bash_unit(input, context, commands, index, depth, span.mode, allow_bracket, text.as_deref_mut())
        };
        match unit {
            Err(reason) => return Err(reason),
            Ok(Some(end)) => {
                index = end + 1;
                continue;
            }
            Ok(None) => {}
        }
        let escapes_boundary = ch == u16::from(b'\\')
            && (Some(input.get(index + 1).copied().unwrap_or(0)) == span.open.map(u16::from)
                || Some(input.get(index + 1).copied().unwrap_or(0)) == span.close.map(u16::from));
        if escapes_boundary {
            if let Some(text) = text.as_deref_mut() {
                text.word.push_str(&units(input, index, index + 2));
                text.literal.push_str(&units(input, index, index + 2));
            }
            index += 2;
            continue;
        }
        if let Some(text) = text.as_deref_mut() {
            append_text_unit(text, ch);
        }
        index += 1;
    }
    if span.close.is_some() { Err(OpaqueReason::UnterminatedQuote) } else { Ok(input.len()) }
}

pub(crate) fn scan_bash_nested(
    input: &[u16],
    context: &mut BashContext,
    commands: &mut Vec<Command>,
    start: usize,
    depth: usize,
    close: Close,
    text: Option<&mut BashText>,
) -> Result<usize, OpaqueReason> {
    let mut heredocs = Vec::new();
    match scan_bash(input, start, depth + 1, context, Some(close), &mut heredocs, false, text) {
        BashResult::Opaque(reason) => Err(reason),
        BashResult::Scanned { commands: nested, end } => {
            commands.extend(nested);
            Ok(end)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn scan_bash_dollar_or_backtick(
    input: &[u16],
    start: usize,
    depth: usize,
    context: &mut BashContext,
    commands: &mut Vec<Command>,
    quoted: bool,
    allow_bracket: bool,
    text: Option<&mut BashText>,
) -> Result<usize, OpaqueReason> {
    if depth >= MAX_SUBSTITUTION_DEPTH {
        return Err(OpaqueReason::CommandSubstitution);
    }
    if input.get(start) == Some(&u16::from(b'`')) {
        return scan_bash_backtick(input, start, depth + 1, context, commands, quoted, text);
    }
    if starts_with(input, start, "$((")
        && let Some(end) = scan_bash_arithmetic(input, context, commands, start + 3, depth + 1, &SPAN_ARITHMETIC)?
    {
        return Ok(end);
    }
    if starts_with(input, start, "$(") {
        return scan_bash_nested(input, context, commands, start + 2, depth, Close::Paren, text);
    }
    if let Some(nofork_end) = no_fork_open_end(input, start) {
        return scan_bash_nested(input, context, commands, nofork_end, depth, Close::Nofork, text);
    }
    if starts_with(input, start, "${") {
        let mut parameter = BashText::default();
        let end = scan_bash_parameter(input, start + 2, depth + 1, context, commands, quoted, &mut parameter)?;
        let indirect = indirect_at(input, start + 2)
            || (context.dialect != Dialect::Bash
                && input.get(start + 2) == Some(&u16::from(b'('))
                && ZSH_SAFE_FLAGS_RE.test_at(input, start)
                && ZSH_SAFE_FLAGS_RE.end_at(input, start).is_some_and(|flags_end| {
                    ZSH_QUOTED_FLAG_ARG_RE.replace_all(&input[start + 3..flags_end - 1], "").contains('P')
                }));
        if indirect {
            context.variable_sink = true;
            if bash_evaluates_expansion(&parameter, BashEvaluation::Binding, context) {
                return Err(OpaqueReason::DynamicExecution);
            }
        }
        if let Some(text) = text {
            text.literal.push_str(&parameter.literal);
            text.output.push_str(&parameter.output);
        }
        return Ok(end);
    }
    if !allow_bracket && context.dialect == Dialect::Posix {
        return Err(OpaqueReason::CommandSubstitution);
    }
    let mut expression = BashText::default();
    let end = scan_bash_span(
        input,
        context,
        commands,
        start + 2,
        depth + 1,
        &SPAN_BRACKET_ARITHMETIC,
        true,
        Some(&mut expression),
    )?;
    if bash_evaluates_expansion(&expression, BashEvaluation::Arithmetic, context) {
        return Err(OpaqueReason::DynamicExecution);
    }
    Ok(end)
}

pub(crate) fn scan_bash_arithmetic(
    input: &[u16],
    context: &mut BashContext,
    commands: &mut Vec<Command>,
    start: usize,
    depth: usize,
    span: &BashSpan,
) -> Step {
    let mut arithmetic = Vec::new();
    let mut text = BashText::default();
    let end = match scan_bash_span(input, context, &mut arithmetic, start, depth, span, true, Some(&mut text)) {
        Ok(end) => end,
        Err(_) => return Err(OpaqueReason::InvalidStructure),
    };
    let close = bash_operator(input, end, &["))"])?;
    if close.is_none() {
        return Ok(None);
    }
    if bash_evaluates_expansion(&text, BashEvaluation::Arithmetic, context) {
        return Err(OpaqueReason::DynamicExecution);
    }
    commands.extend(arithmetic);
    Ok(Some(end + 1))
}

fn scan_bash_backtick(
    input: &[u16],
    start: usize,
    depth: usize,
    context: &mut BashContext,
    commands: &mut Vec<Command>,
    quoted: bool,
    text: Option<&mut BashText>,
) -> Result<usize, OpaqueReason> {
    let mut source: Vec<u16> = Vec::new();
    let mut index = start + 1;
    while index < input.len() {
        context.remaining -= 1;
        if context.remaining < 0 {
            return Err(OpaqueReason::InvalidStructure);
        }
        let ch = input[index];
        if ch == u16::from(b'`') {
            let mut heredocs = Vec::new();
            match scan_bash(&source, 0, depth, context, None, &mut heredocs, false, text) {
                BashResult::Opaque(_) => return Err(OpaqueReason::CommandSubstitution),
                BashResult::Scanned { commands: inner, .. } => commands.extend(inner),
            }
            return Ok(index);
        }
        let next = input.get(index + 1).copied().unwrap_or(0);
        if ch == u16::from(b'\\') && (matches!(next, 0x24 | 0x60 | 0x5c | 0x0a) || (quoted && next == u16::from(b'"')))
        {
            index += 1;
            if next != u16::from(b'\n') {
                source.push(next);
            }
            index += 1;
            continue;
        }
        source.push(ch);
        index += 1;
    }
    Err(OpaqueReason::CommandSubstitution)
}

fn scan_bash_parameter(
    input: &[u16],
    start: usize,
    depth: usize,
    context: &mut BashContext,
    commands: &mut Vec<Command>,
    quoted: bool,
    text: &mut BashText,
) -> Result<usize, OpaqueReason> {
    let name_start = parameter_prefix_end(input, start);
    let mut colon = parameter_name_end(input, name_start);
    let name = match colon {
        Some(end) if end > name_start => units(input, name_start, end).replace("\\\n", ""),
        _ => String::new(),
    };
    if context.dialect != Dialect::Bash
        && (NULLCMD_EXACT_RE.is_match_str(&name)
            || (OPTION_NAME_EXACT_RE.is_match_str(&name)
                && colon.and_then(|at| input.get(at)).is_some_and(|unit| matches!(unit, 0x5b | 0x3d | 0x3a))))
    {
        return Err(OpaqueReason::DynamicExecution);
    }
    if name == "PS4" {
        context.eval_sink = true;
    }
    let mut evaluation: Option<BashEvaluation> = None;
    let mut word_offset = 0;
    let mut literal_offset = 0;
    let mut index = start;
    while index < input.len() {
        context.remaining -= 1;
        if context.remaining < 0 {
            return Err(OpaqueReason::InvalidStructure);
        }
        let ch = input[index];
        if ch == u16::from(b'}') {
            if let Some(evaluation) = evaluation {
                let tail = BashText {
                    word: slice_units(&text.word, word_offset, None),
                    literal: slice_units(&text.literal, literal_offset, None),
                    output: text.output.clone(),
                };
                if bash_evaluates_expansion(&tail, evaluation, context) {
                    return Err(OpaqueReason::DynamicExecution);
                }
            }
            return Ok(index);
        }
        if Some(index) == colon && evaluation.is_none() {
            let next = input.get(index + 1).copied().unwrap_or(0);
            let next_char = char::from_u32(u32::from(next));
            if ch == u16::from(b':') && !matches!(next_char, Some('-' | '=' | '?' | '+')) {
                evaluation = Some(BashEvaluation::Arithmetic);
            }
            if ch == u16::from(b'=') || (ch == u16::from(b':') && next == u16::from(b'=')) {
                evaluation = Some(BashEvaluation::Deferred);
            }
            if evaluation.is_some() {
                word_offset = unit_len(&text.word);
                literal_offset = unit_len(&text.literal);
            }
        }
        if ch == u16::from(b'@') && parameter_prompt_at(input, index) {
            return Err(OpaqueReason::DynamicExecution);
        }
        if !quoted && context.dialect != Dialect::Bash && ch == u16::from(b'(') && index > start {
            return Err(OpaqueReason::DynamicExecution);
        }
        if quoted && ch == u16::from(b'\'') && input.get(index.wrapping_sub(1)) == Some(&u16::from(b'$')) {
            let ansi = bash_ansi_quote(input, index, context.dialect, true)?;
            text.literal.push_str(&ansi.value);
        }
        let subscript = Some(index) == colon && ch == u16::from(b'[');
        let unit = if subscript {
            Ok(Some(scan_bash_subscript(input, context, commands, index + 1, depth, &SPAN_SUBSCRIPT, None)?))
        } else if quoted && ch == u16::from(b'\'') {
            Ok(Some(scan_bash_span(
                input,
                context,
                commands,
                index + 1,
                depth,
                &SPAN_PARAMETER_QUOTE,
                false,
                Some(&mut *text),
            )?))
        } else if quoted && ch == u16::from(b'"') {
            Ok(Some(scan_bash_span(input, context, commands, index + 1, depth, &SPAN_DOUBLE, false, Some(&mut *text))?))
        } else {
            scan_bash_unit(
                input,
                context,
                commands,
                index,
                depth,
                if quoted { TextMode::Quoted } else { TextMode::Word },
                false,
                Some(&mut *text),
            )
        };
        let unit = unit?;
        if quoted
            && unit.is_some()
            && starts_with(input, index, "$(")
            && input.get(index..unit.unwrap() + 1).is_some_and(|slice| slice.contains(&u16::from(b'}')))
        {
            return Err(OpaqueReason::CommandSubstitution);
        }
        if (subscript || (index == name_start && starts_with(input, index, "${") && unit.is_some()))
            && let Some(unit) = unit
        {
            let at = unit + 1;
            colon = Some(line_continuation_end(input, at));
        }
        if let Some(unit) = unit {
            index = unit + 1;
            continue;
        }
        append_text_unit(text, ch);
        index += 1;
    }
    Err(OpaqueReason::CommandSubstitution)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn scan_bash_conditional(
    input: &[u16],
    context: &mut BashContext,
    commands: &mut Vec<Command>,
    start: usize,
    depth: usize,
) -> Result<usize, OpaqueReason> {
    let mut words: Vec<BashText> = Vec::new();
    let mut text = BashText::default();
    let mut word_started = false;
    let mut has_word = false;
    let mut paren_depth: i64 = 0;
    let mut pattern_paren_depth: i64 = 0;
    let mut index = start;
    while index < input.len() {
        context.remaining -= 1;
        if context.remaining < 0 {
            return Err(OpaqueReason::InvalidStructure);
        }
        let ch = input[index];
        if ch == u16::from(b'\\') && input.get(index + 1) == Some(&u16::from(b'\n')) {
            index += 2;
            continue;
        }
        if !word_started && has_word {
            words.push(std::mem::take(&mut text));
            has_word = false;
        }
        let close = if !word_started && ch == u16::from(b']') { bash_operator(input, index, &["]]"])? } else { None };
        if close.is_some() && brace_close_ahead(input, index + 2) {
            if paren_depth != 0 {
                return Err(OpaqueReason::InvalidStructure);
            }
            let evaluated = words.iter().enumerate().any(|(at, word)| {
                let previous =
                    at.checked_sub(1).and_then(|position| words.get(position)).map(|word| word.word.as_str());
                let next = words.get(at + 1).map(|word| word.word.as_str());
                (previous == Some("-v") && bash_evaluates_expansion(word, BashEvaluation::Binding, context))
                    || (((context.dialect != Dialect::Bash && previous == Some("-t"))
                        || previous.is_some_and(|word| BASH_ARITHMETIC_COMPARISONS.contains(word))
                        || next.is_some_and(|word| BASH_ARITHMETIC_COMPARISONS.contains(word)))
                        && bash_evaluates_expansion(word, BashEvaluation::Arithmetic, context))
            });
            if evaluated {
                return Err(OpaqueReason::DynamicExecution);
            }
            return Ok(index + 1);
        }
        if !word_started && ch == u16::from(b'#') {
            let Some(newline) = index_of(input, index, "\n") else {
                return Err(OpaqueReason::InvalidStructure);
            };
            index = newline + 1;
            continue;
        }
        if matches!(ch, 0x20 | 0x09 | 0x0a) {
            if pattern_paren_depth == 0 {
                word_started = false;
            }
            index += 1;
            continue;
        }
        if ch == u16::from(b';') {
            return Err(OpaqueReason::InvalidStructure);
        }
        if ch == u16::from(b'(') || ch == u16::from(b')') {
            if ch == u16::from(b')') && paren_depth == 0 {
                return Err(OpaqueReason::InvalidStructure);
            }
            let in_pattern = pattern_paren_depth > 0
                || (ch == u16::from(b'(')
                    && words.last().is_some_and(|word| matches!(word.word.as_str(), "=" | "==" | "!=" | "=~")));
            paren_depth += if ch == u16::from(b'(') { 1 } else { -1 };
            if in_pattern {
                pattern_paren_depth += if ch == u16::from(b'(') { 1 } else { -1 };
                word_started = true;
                has_word = true;
                append_text_unit(&mut text, ch);
                index += 1;
                continue;
            }
            if has_word {
                words.push(std::mem::take(&mut text));
                has_word = false;
            }
            words.push(BashText {
                word: char::from_u32(u32::from(ch)).map(String::from).unwrap_or_default(),
                literal: String::new(),
                output: String::new(),
            });
            word_started = false;
            index += 1;
            continue;
        }
        let unit = scan_bash_unit(input, context, commands, index, depth, TextMode::Word, true, Some(&mut text));
        let unit = unit?;
        if unit.is_none() && pattern_paren_depth == 0 && matches!(ch, 0x26 | 0x7c | 0x3c | 0x3e) {
            if has_word {
                words.push(std::mem::take(&mut text));
                has_word = false;
            }
            words.push(BashText {
                word: char::from_u32(u32::from(ch)).map(String::from).unwrap_or_default(),
                literal: String::new(),
                output: String::new(),
            });
            word_started = false;
            index += 1;
            continue;
        }
        word_started = true;
        has_word = true;
        if let Some(unit) = unit {
            index = unit + 1;
            continue;
        }
        append_text_unit(&mut text, ch);
        index += 1;
    }
    Err(OpaqueReason::InvalidStructure)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn scan_bash_array_or_pattern(
    input: &[u16],
    context: &mut BashContext,
    commands: &mut Vec<Command>,
    start: usize,
    depth: usize,
    mode: super::ArrayMode,
) -> Result<usize, OpaqueReason> {
    if depth > MAX_SUBSTITUTION_DEPTH {
        return Err(OpaqueReason::CommandSubstitution);
    }
    let mut text = (mode == ArrayMode::Array).then(BashText::default);
    let mut word_started = false;
    let mut index = start + 1;
    while index < input.len() {
        context.remaining -= 1;
        if context.remaining < 0 {
            return Err(OpaqueReason::InvalidStructure);
        }
        let ch = input[index];
        if ch == u16::from(b'\\') && input.get(index + 1) == Some(&u16::from(b'\n')) {
            index += 2;
            continue;
        }
        if let Some(text) = text.as_mut()
            && matches!(ch, 0x29 | 0x20 | 0x09 | 0x0a)
        {
            bash_evaluates_expansion(text, BashEvaluation::Deferred, context);
            text.literal.clear();
            text.output.clear();
        }
        if ch == u16::from(b')') {
            return Ok(index);
        }
        if mode == ArrayMode::Array && !word_started && ch == u16::from(b'#') {
            let Some(newline) = index_of(input, index, "\n") else {
                return Err(OpaqueReason::CommandSubstitution);
            };
            index = newline + 1;
            continue;
        }
        if matches!(ch, 0x20 | 0x09) || (mode == ArrayMode::Array && ch == 0x0a) {
            word_started = false;
            index += 1;
            continue;
        }
        if mode == ArrayMode::Array
            && word_started
            && ch == u16::from(b'(')
            && context.dialect == Dialect::Zsh
            && ZSH_GLOB_QUALIFIER_RE.test_at(input, index)
        {
            let end = ZSH_GLOB_QUALIFIER_RE.end_at(input, index).unwrap_or(index) - 1;
            if let Some(text) = text.as_mut() {
                text.literal.push_str(&units(input, index, end + 1));
            }
            index = end + 1;
            continue;
        }
        if matches!(ch, 0x0a | 0x3b | 0x26) || (mode == ArrayMode::Array && matches!(ch, 0x7c | 0x28)) {
            return Err(OpaqueReason::CommandSubstitution);
        }
        word_started = true;
        let unit =
            if mode == ArrayMode::Array && ch == u16::from(b'=') && input.get(index + 1) == Some(&u16::from(b'(')) {
                Ok(Some(scan_bash_nested(input, context, commands, index + 2, depth, Close::Paren, None)?))
            } else if ch == u16::from(b'(') {
                Ok(Some(scan_bash_array_or_pattern(input, context, commands, index, depth + 1, mode)?))
            } else if ch == u16::from(b'[') {
                Ok(Some(scan_bash_subscript(input, context, commands, index + 1, depth, &SPAN_SUBSCRIPT, None)?))
            } else {
                scan_bash_unit(input, context, commands, index, depth, TextMode::Word, false, text.as_mut())
            };
        let unit = unit?;
        if let Some(unit) = unit {
            index = unit + 1;
            continue;
        }
        if let Some(text) = text.as_mut() {
            append_text_unit(text, ch);
        }
        index += 1;
    }
    Err(OpaqueReason::CommandSubstitution)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn scan_bash_unit(
    input: &[u16],
    context: &mut BashContext,
    commands: &mut Vec<Command>,
    index: usize,
    depth: usize,
    mode: TextMode,
    allow_bracket: bool,
    mut text: Option<&mut BashText>,
) -> Step {
    let ch = input[index];
    let next = input.get(index + 1).copied().unwrap_or(0);
    if ch == u16::from(b'\\') {
        if mode == TextMode::Quoted && !matches!(next, 0x24 | 0x60 | 0x22 | 0x5c | 0x0a) {
            return Ok(None);
        }
        if let Some(text) = text.as_deref_mut()
            && next != u16::from(b'\n')
        {
            append_text(text, &units(input, index + 1, index + 2));
        }
        return Ok(Some(index + 1));
    }
    if ch == u16::from(b'$') && context.dialect != Dialect::Bash && zsh_evaluates(input, index) {
        return Err(OpaqueReason::DynamicExecution);
    }
    if ch == u16::from(b'$')
        && context.dialect != Dialect::Bash
        && matches!(mode, TextMode::Word | TextMode::Quoted)
        && zsh_subscript_end(input, index).is_some()
    {
        let bracket = zsh_subscript_end(input, index).unwrap_or(index + 1);
        let mut subscript = BashText::default();
        let span = if mode == TextMode::Quoted { &SPAN_QUOTED_SUBSCRIPT } else { &SPAN_SUBSCRIPT };
        let end = scan_bash_subscript(input, context, commands, bracket, depth, span, Some(&mut subscript))?;
        if let Some(text) = text.as_deref_mut() {
            text.word.push_str(&units(input, index, bracket));
            text.word.push_str(&subscript.word);
            text.word.push(']');
        }
        return Ok(Some(end));
    }
    let opener = if ch == u16::from(b'$') {
        bash_operator(input, index, &["$$", "$((", "$(", "${", "$[", "$'", "$\""])?
    } else if mode == TextMode::Word && matches!(ch, 0x3c | 0x3e) {
        let candidate = if ch == u16::from(b'<') { "<(" } else { ">(" };
        bash_operator(input, index, &[candidate])?
    } else {
        None
    };
    if opener == Some("$$") {
        let quote_start = line_continuation_end(input, index + 2);
        if input.get(quote_start) == Some(&u16::from(b'\'')) {
            let Some(next_quote) = index_of(input, quote_start + 1, "'") else {
                return Err(OpaqueReason::UnterminatedQuote);
            };
            let literal = input.get(quote_start + 1..next_quote).is_some_and(|slice| slice.contains(&u16::from(b'\\')));
            if literal && index_of(input, next_quote + 1, "'").is_some() {
                return Err(OpaqueReason::UnterminatedQuote);
            }
        }
        if let Some(text) = text.as_deref_mut() {
            text.word.push_str("$$");
        }
        return Ok(Some(index + 1));
    }
    if mode == TextMode::Word && opener == Some("$'") {
        let quote = bash_ansi_quote(input, index + 1, context.dialect, true)?;
        let first_quote = index_of(input, index + 2, "'");
        if first_quote != Some(quote.end) {
            let dash_dollar =
                context.dialect != Dialect::Bash && index >= 2 && input.get(index - 2) == Some(&u16::from(b'$'));
            let posix_ambiguous = context.dialect == Dialect::Posix
                && first_quote.is_some_and(|first| {
                    index_of(input, first + 1, "'").is_some_and(|next| next < quote.end)
                        || ANSI_QUOTE_UNSAFE_RE.is_match(input.get(first + 1..).unwrap_or(&[]))
                });
            if dash_dollar || posix_ambiguous {
                return Err(OpaqueReason::UnterminatedQuote);
            }
        }
        if let Some(text) = text.as_deref_mut() {
            append_text(text, &quote.value);
        }
        return Ok(Some(quote.end));
    }
    if mode == TextMode::Arithmetic && opener == Some("$'") {
        let quote =
            bash_ansi_quote(input, index + 1, context.dialect, true).map_err(|_| OpaqueReason::InvalidStructure)?;
        if index_of(input, index + 2, "'") != Some(quote.end)
            || DISALLOWED_ARITHMETIC_QUOTE_RE.is_match_str(&quote.value)
        {
            return Err(OpaqueReason::InvalidStructure);
        }
        if let Some(text) = text.as_deref_mut() {
            append_text(text, &quote.value);
        }
        return Ok(Some(scan_bash_span(
            input,
            context,
            commands,
            index + 2,
            depth,
            &SPAN_ARITHMETIC_QUOTE,
            allow_bracket,
            text,
        )?));
    }
    if mode == TextMode::Word && ch == u16::from(b'\'') {
        let Some(end) = index_of(input, index + 1, "'") else {
            return Err(OpaqueReason::UnterminatedQuote);
        };
        if let Some(text) = text.as_deref_mut() {
            append_text(text, &units(input, index + 1, end));
        }
        return Ok(Some(end));
    }
    if mode == TextMode::Arithmetic && ch == u16::from(b'\'') {
        return Ok(Some(scan_bash_span(
            input,
            context,
            commands,
            index + 1,
            depth,
            &SPAN_ARITHMETIC_QUOTE,
            allow_bracket,
            text,
        )?));
    }
    if mode == TextMode::Word && opener == Some("$\"") {
        return Ok(Some(scan_bash_span(
            input,
            context,
            commands,
            index + 2,
            depth,
            &SPAN_DOUBLE,
            allow_bracket,
            text,
        )?));
    }
    if mode != TextMode::Quoted && ch == u16::from(b'"') {
        let span = if mode == TextMode::Arithmetic { &SPAN_ARITHMETIC_DOUBLE } else { &SPAN_DOUBLE };
        return Ok(Some(scan_bash_span(input, context, commands, index + 1, depth, span, allow_bracket, text)?));
    }
    let end = if opener == Some("<(") || opener == Some(">(") {
        Some(scan_bash_nested(input, context, commands, index + 2, depth, Close::Paren, None)?)
    } else if (opener.is_some_and(|opener| opener != "$'" && opener != "$\"")) || ch == u16::from(b'`') {
        Some(scan_bash_dollar_or_backtick(
            input,
            index,
            depth,
            context,
            commands,
            mode != TextMode::Word,
            allow_bracket,
            text.as_deref_mut(),
        )?)
    } else {
        None
    };
    if end.is_none() && ch == u16::from(b'$') && is_parameter_start(next) {
        if let Some(text) = text.as_deref_mut() {
            text.word.push('$');
        }
        return Ok(Some(index));
    }
    if let (Some(text), Some(end)) = (text, end) {
        text.word.push_str(&units(input, index, end + 1));
    }
    Ok(end)
}

fn is_parameter_start(unit: u16) -> bool {
    let Some(ch) = char::from_u32(u32::from(unit)) else {
        return false;
    };
    ch.is_ascii_alphanumeric() || ch == '_' || "@*#?$!-".contains(ch)
}

pub(crate) fn bash_ansi_quote(
    input: &[u16],
    start: usize,
    dialect: Dialect,
    allow_unicode: bool,
) -> Result<AnsiQuote, OpaqueReason> {
    let mut value = String::new();
    let mut index = start + 1;
    while index < input.len() {
        let ch = input[index];
        if ch == u16::from(b'\'') {
            return Ok(AnsiQuote { value, end: index });
        }
        if ch != u16::from(b'\\') {
            push_unit(&mut value, ch);
            index += 1;
            continue;
        }
        index += 1;
        let Some(escaped) = input.get(index).copied() else {
            break;
        };
        let escaped_char = char::from_u32(u32::from(escaped)).unwrap_or('\0');
        let mapped = match escaped_char {
            'a' => Some('\x07'),
            'b' => Some('\x08'),
            'e' | 'E' => Some('\x1b'),
            'f' => Some('\x0c'),
            'n' => Some('\n'),
            'r' => Some('\r'),
            't' => Some('\t'),
            'v' => Some('\x0b'),
            '\\' => Some('\\'),
            '\'' => Some('\''),
            '"' => Some('"'),
            '?' => Some('?'),
            _ => None,
        };
        if let Some(mapped) = mapped {
            value.push(mapped);
            index += 1;
            continue;
        }
        let digits = if escaped_char == 'x' {
            leading_hex(input, index + 1, index + 3, 2)
        } else if allow_unicode && escaped_char == 'u' {
            leading_hex(input, index + 1, index + 5, 4)
        } else if allow_unicode && escaped_char == 'U' {
            leading_hex(input, index + 1, index + 9, 8)
        } else if escaped_char.is_ascii_digit() && ('0'..='7').contains(&escaped_char) {
            leading_octal(input, index, index + 3, 3)
        } else {
            None
        };
        if let Some(digits) = digits {
            let octal = ('0'..='7').contains(&escaped_char);
            let point = u32::from_str_radix(&digits, if octal { 8 } else { 16 }).unwrap_or(0);
            if point == 0 || point > 0x10ffff || (0xd800..=0xdfff).contains(&point) {
                return Err(OpaqueReason::UnsupportedEscape);
            }
            value.push(char::from_u32(point).unwrap());
            let length = digits.len();
            index += length - if octal { 1 } else { 0 };
            index += 1;
            continue;
        }
        if dialect == Dialect::Posix
            || (dialect == Dialect::Bash
                && (escaped_char == 'c' || (!allow_unicode && matches!(escaped_char, 'u' | 'U'))))
            || (dialect == Dialect::Zsh && matches!(escaped_char, 'C' | 'M' | 'x' | 'u' | 'U'))
        {
            return Err(OpaqueReason::UnsupportedEscape);
        }
        if dialect == Dialect::Bash {
            value.push('\\');
        }
        value.push(escaped_char);
        index += 1;
    }
    Err(OpaqueReason::UnterminatedQuote)
}

fn leading_hex(input: &[u16], from: usize, to: usize, max: usize) -> Option<String> {
    leading_digits(input, from, to, max, |ch| ch.is_ascii_hexdigit())
}

fn leading_octal(input: &[u16], from: usize, to: usize, max: usize) -> Option<String> {
    leading_digits(input, from, to, max, |ch| ('0'..='7').contains(&ch))
}

fn leading_digits(input: &[u16], from: usize, to: usize, max: usize, accept: impl Fn(char) -> bool) -> Option<String> {
    let mut digits = String::new();
    for unit in &input[from..to.min(input.len())] {
        let Some(ch) = char::from_u32(u32::from(*unit)) else {
            break;
        };
        if digits.len() >= max || !accept(ch) {
            break;
        }
        digits.push(ch);
    }
    (!digits.is_empty()).then_some(digits)
}

pub(crate) fn bash_heredoc_delimiter(
    input: &[u16],
    start: usize,
    tabs: bool,
    dialect: Dialect,
) -> Option<HeredocDelimiter> {
    let mut delimiter = String::new();
    let mut dash_delimiter: Option<String> = None;
    let mut quoted = false;
    let mut quote: Option<u16> = None;
    let mut end = start;
    let mut started = false;
    let mut index = start;
    while index < input.len() {
        let ch = input[index];
        if !started && matches!(ch, 0x20 | 0x09) {
            index += 1;
            continue;
        }
        if !started && ch == u16::from(b'#') {
            return None;
        }
        if quote.is_none() && matches!(ch, 0x20 | 0x09 | 0x0a | 0x3b | 0x26 | 0x7c | 0x28 | 0x29 | 0x3c | 0x3e) {
            return started.then_some(HeredocDelimiter { delimiter, dash_delimiter, quoted, tabs, end });
        }
        if quote.is_none() && ch == u16::from(b'$') && starts_with(input, index + 1, "\\\n") {
            return None;
        }
        if quote.is_none() && starts_with(input, index, "$'") {
            let literal = bash_ansi_quote(input, index + 1, dialect, false).ok()?;
            if index_of(input, index + 2, "'") != Some(literal.end) {
                return None;
            }
            let previous = dash_delimiter.take().unwrap_or_else(|| delimiter.clone());
            dash_delimiter = Some(format!("{previous}${}", units(input, index + 2, literal.end)));
            delimiter.push_str(&literal.value);
            quoted = true;
            started = true;
            index = literal.end + 1;
            end = literal.end;
            continue;
        }
        if quote.is_none() && starts_with(input, index, "$\"") {
            return None;
        }
        if Some(ch) == quote {
            quote = None;
            end = index;
            index += 1;
            continue;
        }
        if ch == u16::from(b'\\') && quote != Some(u16::from(b'\'')) {
            let next = input.get(index + 1).copied()?;
            if next == u16::from(b'\n') {
                index += 2;
                continue;
            }
            if quote.is_none() || matches!(next, 0x24 | 0x60 | 0x22 | 0x5c) {
                quoted = true;
                started = true;
                if let Some(dash) = dash_delimiter.as_mut() {
                    dash.push(next as u8 as char);
                }
                push_unit(&mut delimiter, next);
                end = index + 1;
                index += 2;
                continue;
            }
        }
        if quote.is_none() && (ch == u16::from(b'\'') || ch == u16::from(b'"')) {
            quote = Some(ch);
            quoted = true;
            started = true;
            end = index;
            index += 1;
            continue;
        }
        if let Some(dash) = dash_delimiter.as_mut() {
            push_unit(dash, ch);
        }
        push_unit(&mut delimiter, ch);
        started = true;
        end = index;
        index += 1;
    }
    (started && quote.is_none()).then_some(HeredocDelimiter { delimiter, dash_delimiter, quoted, tabs, end })
}

pub(crate) fn bash_heredoc(input: &[u16], start: usize, delimiter: &BashHeredoc) -> Option<HeredocBody> {
    let body_start = start;
    let mut line_start = start;
    let mut line = String::new();
    let mut backslashes: usize = 0;
    let mut start = start;
    let mut index = start;
    while index <= input.len() {
        if index < input.len() && input[index] != u16::from(b'\n') {
            index += 1;
            continue;
        }
        let text = if delimiter.tabs && start == line_start {
            strip_leading_tabs(units(input, start, index))
        } else {
            units(input, start, index)
        };
        let bytes = text.as_bytes();
        let mut run = 0;
        while run < bytes.len() && bytes[bytes.len() - 1 - run] == b'\\' {
            run += 1;
        }
        backslashes = if run == unit_len(&text) { backslashes + run } else { run };
        let continued = !delimiter.quoted && backslashes % 2 == 1 && index < input.len();
        let max_length =
            unit_len(&delimiter.delimiter).max(delimiter.dash_delimiter.as_deref().map(unit_len).unwrap_or(0));
        if unit_len(&line) <= max_length {
            if continued {
                line.push_str(&text[..text.len().saturating_sub(1)]);
            } else {
                line.push_str(&text);
            }
        }
        if continued {
            backslashes -= 1;
            start = index + 1;
            index += 1;
            continue;
        }
        if delimiter.dash_delimiter.as_deref() == Some(line.as_str()) {
            return None;
        }
        if line == delimiter.delimiter {
            if start > line_start && contains(input, index + 1, &delimiter.delimiter) {
                return None;
            }
            return Some(HeredocBody { source: units(input, body_start, line_start), end: index });
        }
        line.clear();
        backslashes = 0;
        start = index + 1;
        line_start = start;
        index += 1;
    }
    None
}

fn strip_leading_tabs(text: String) -> String {
    text.trim_start_matches('\t').to_string()
}
