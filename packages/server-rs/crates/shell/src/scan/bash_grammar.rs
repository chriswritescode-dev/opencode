use super::bash::{
    BashState, Phase, Structure, StructureKind, at_command_start, bash_command_key, bash_crosses_heredoc,
    bash_in_header, bash_operator, bash_statement, end_bash_list, finish_bash_command, finish_bash_word, scan_bash,
};
use super::bash_words::{
    SPAN_ARITHMETIC, SPAN_FOR_ARITHMETIC, scan_bash_arithmetic, scan_bash_array_or_pattern, scan_bash_conditional,
};
use super::*;

pub(crate) fn scan_bash_command_start(state: &mut BashState, index: usize, depth: usize) -> Step {
    let input = state.input;
    let ch = input[index];
    if state.structures.last().is_some_and(|structure| structure.phase == Phase::Pattern && !structure.pattern_started)
        && state.words.is_empty()
        && ch == b'('.into()
    {
        if let Some(structure) = state.structures.last_mut() {
            structure.pattern_started = true;
        }
        return Ok(Some(index));
    }
    if state.structures.last().is_some_and(|structure| structure.phase == Phase::Pattern) && ch == 0x7c {
        if state.words.is_empty() {
            return Err(OpaqueReason::CompoundCommand);
        }
        finish_bash_word(state);
        if let Some(structure) = state.structures.last_mut() {
            structure.pattern_started = true;
        }
        return Ok(Some(index));
    }
    if state.structures.last().is_some_and(|structure| structure.phase == Phase::Pattern) && ch == b')'.into() {
        if state.words.is_empty() {
            return Err(OpaqueReason::CompoundCommand);
        }
        finish_bash_command(state, false, false);
        if let Some(structure) = state.structures.last_mut() {
            structure.phase = Phase::Body;
            structure.count = 0;
        }
        return Ok(Some(index));
    }
    if state
        .structures
        .last()
        .is_some_and(|structure| structure.kind == StructureKind::For && structure.phase == Phase::Header)
        && ch == b'('.into()
        && input.get(index + 1) != Some(&b'('.into())
    {
        let end = scan_bash_array_or_pattern(
            input,
            &mut *state.context,
            &mut state.nested_commands,
            index,
            depth + 1,
            ArrayMode::Array,
        )?;
        if bash_crosses_heredoc(state, index, end) {
            return Err(OpaqueReason::Heredoc);
        }
        finish_bash_command(state, false, false);
        if let Some(structure) = state.structures.last_mut() {
            structure.phase = Phase::Do;
            structure.parenthesized = true;
            if !DO_AHEAD_RE.test_at(input, end + 1) {
                state.structures.pop();
            }
        }
        return Ok(Some(end));
    }
    if state
        .structures
        .last()
        .is_some_and(|structure| structure.kind == StructureKind::Repeat && structure.phase == Phase::Do)
        && !matches!(ch, 0x0a | 0x23 | 0x3b)
        && token_at(input, index).map(|range| units(input, range.start, range.end)).as_deref() != Some("do")
    {
        state.structures.pop();
    }
    if let Some(step) = scan_bash_keyword(state, index, depth)? {
        return Ok(Some(step));
    }
    if state
        .structures
        .last()
        .is_some_and(|structure| structure.kind == StructureKind::For && structure.phase == Phase::Do)
        && !matches!(ch, 0x0a | 0x23 | 0x3b)
    {
        if ch != b'{'.into() || !space_continuation_ahead(input, index + 1) {
            return Err(OpaqueReason::CompoundCommand);
        }
        state.structures.pop();
    }
    let at_start = at_command_start(state);
    let function_head = if at_start && ch == b'('.into() { bash_function_head_length(input, index) } else { 0 };
    if function_head > 0 {
        return Ok(Some(index + function_head - 1));
    }
    if at_start && ch == b'!'.into() && negation_ahead(input, index + 1) {
        return Ok(Some(index));
    }
    let for_header = state
        .structures
        .last()
        .is_some_and(|structure| structure.kind == StructureKind::For && structure.phase == Phase::Header);
    let compound_open = if (at_start || for_header) && matches!(ch, 0x28 | 0x5b) {
        bash_operator(input, index, &["((", "[["])?
    } else {
        None
    };
    if compound_open == Some("((") {
        if for_header && !state.words.is_empty() {
            return Err(OpaqueReason::CompoundCommand);
        }
        let span = if for_header { &SPAN_FOR_ARITHMETIC } else { &SPAN_ARITHMETIC };
        let end = scan_bash_arithmetic(input, &mut *state.context, &mut state.commands, index + 2, depth, span)?;
        if let Some(end) = end {
            if bash_crosses_heredoc(state, index, end) {
                return Err(OpaqueReason::Heredoc);
            }
            if !for_header && state.context.dialect == Dialect::Posix {
                let mut subshell_heredocs = Vec::new();
                let subshells = scan_bash(
                    input,
                    index + 1,
                    depth + 1,
                    state.context,
                    Some(Close::Paren),
                    &mut subshell_heredocs,
                    false,
                    None,
                );
                let subshells = match subshells {
                    BashResult::Opaque(reason) => return Err(reason),
                    BashResult::Scanned { commands, end } => (commands, end),
                };
                if subshells.1 != end {
                    return Err(OpaqueReason::CompoundCommand);
                }
                let seen: HashSet<(String, Vec<String>)> = state.commands.iter().map(bash_command_key).collect();
                state
                    .commands
                    .extend(subshells.0.into_iter().filter(|command| !seen.contains(&bash_command_key(command))));
            }
            if for_header && let Some(structure) = state.structures.last_mut() {
                structure.phase = Phase::Do;
                structure.saw_in = true;
            }
            if !for_header {
                bash_statement(state);
                state.compound_end = true;
            }
            return Ok(Some(end));
        }
        if for_header {
            return Err(OpaqueReason::InvalidStructure);
        }
    }
    if at_start && (ch == b'('.into() || (ch == b'{'.into() && space_continuation_ahead(input, index + 1))) {
        let close = if ch == b'{'.into() { Close::Brace } else { Close::Paren };
        let group =
            scan_bash(input, index + 1, depth + 1, state.context, Some(close), &mut *state.heredocs, true, None);
        let group = match group {
            BashResult::Opaque(reason) => return Err(reason),
            BashResult::Scanned { commands, end } => (commands, end),
        };
        if trim_units(input, index + 1, group.1).is_empty() {
            return Err(OpaqueReason::InvalidStructure);
        }
        state.commands.extend(group.0);
        bash_statement(state);
        state.compound_end = true;
        return Ok(Some(group.1));
    }
    if at_start && compound_open == Some("[[") && space_continuation_ahead(input, index + 2) {
        let end = scan_bash_conditional(input, &mut *state.context, &mut state.commands, index + 2, depth)?;
        if bash_crosses_heredoc(state, index, end) {
            return Err(OpaqueReason::Heredoc);
        }
        bash_statement(state);
        state.compound_end = true;
        return Ok(Some(end));
    }
    Ok(None)
}

pub(crate) fn scan_bash_keyword(state: &mut BashState, index: usize, depth: usize) -> Step {
    let input = state.input;
    let ch = input[index];
    let Some(ch) = char::from_u32(u32::from(ch)) else {
        return Ok(None);
    };
    if !(ch.is_ascii_alphabetic() || ch == '_') {
        return Ok(None);
    }
    let structure = state.structures.last().copied();
    let token_range = token_at(input, index);
    let token = token_range.clone().map(|range| units(input, range.start, range.end));
    let end = token_range.map(|range| range.end - 1).unwrap_or(index);
    if let Some(structure) = structure
        && structure.kind == StructureKind::Case
        && structure.phase == Phase::Header
        && token.as_deref() == Some("in")
    {
        if state.words.len() != 1 || state.has_redirect {
            return Err(OpaqueReason::CompoundCommand);
        }
        finish_bash_command(state, false, false);
        if let Some(structure) = state.structures.last_mut() {
            structure.phase = Phase::Pattern;
            structure.pattern_started = false;
        }
        return Ok(Some(end));
    }
    if let Some(structure) = structure
        && structure.kind == StructureKind::For
        && matches!(structure.phase, Phase::Header | Phase::Do)
        && !structure.saw_in
        && !structure.parenthesized
        && token.as_deref() == Some("in")
        && (structure.phase == Phase::Do || !state.words.is_empty())
    {
        if let Some(structure) = state.structures.last_mut() {
            structure.saw_in = true;
            structure.phase = Phase::Header;
        }
        return Ok(Some(end));
    }
    if let Some(structure) = structure
        && structure.phase == Phase::Pattern
        && !structure.pattern_started
        && state.words.is_empty()
        && token.as_deref() == Some("esac")
    {
        state.structures.pop();
        bash_statement(state);
        state.compound_end = true;
        return Ok(Some(end));
    }
    if let Some(structure) = structure
        && structure.kind == StructureKind::For
        && structure.phase == Phase::Header
        && !structure.saw_in
        && state.words.len() == 1
        && !state.has_redirect
        && token.as_deref() == Some("do")
    {
        finish_bash_command(state, false, false);
        if let Some(structure) = state.structures.last_mut() {
            structure.phase = Phase::Do;
        }
    }
    let in_header = bash_in_header(state);
    let keyword = matches!(token.as_deref(), Some("then" | "elif" | "else" | "fi" | "do" | "done" | "esac"));
    if keyword
        && state.words.is_empty()
        && !state.redirect_target
        && (!in_header
            || (token.as_deref() == Some("do")
                && state.structures.last().is_some_and(|structure| structure.phase == Phase::Do)))
    {
        if (state.has_redirect && state.context.dialect != Dialect::Zsh)
            || !end_bash_list(state)
            || state.structures.is_empty()
        {
            return Err(OpaqueReason::CompoundCommand);
        }
        let token = token.as_deref().unwrap_or("");
        if token == "then" {
            let valid = state.structures.last().is_some_and(|structure| {
                structure.kind == StructureKind::If && structure.phase == Phase::Condition && structure.count > 0
            });
            if !valid {
                return Err(OpaqueReason::CompoundCommand);
            }
            if let Some(structure) = state.structures.last_mut() {
                structure.phase = Phase::Body;
                structure.count = 0;
            }
            return Ok(Some(end));
        }
        if token == "elif" || token == "else" {
            let valid = state.structures.last().is_some_and(|structure| {
                structure.kind == StructureKind::If
                    && structure.phase == Phase::Body
                    && structure.count > 0
                    && !structure.saw_else
            });
            if !valid {
                return Err(OpaqueReason::CompoundCommand);
            }
            if let Some(structure) = state.structures.last_mut() {
                structure.phase = if token == "elif" { Phase::Condition } else { Phase::Body };
                structure.saw_else = token == "else";
                structure.count = 0;
            }
            return Ok(Some(end));
        }
        if token == "do" {
            let valid = state.structures.last().is_some_and(|structure| {
                matches!(
                    structure.kind,
                    StructureKind::For | StructureKind::While | StructureKind::Until | StructureKind::Repeat
                ) && matches!(structure.phase, Phase::Condition | Phase::Do)
                    && (matches!(structure.kind, StructureKind::For | StructureKind::Repeat) || structure.count > 0)
            });
            if !valid {
                return Err(OpaqueReason::CompoundCommand);
            }
            if let Some(structure) = state.structures.last_mut() {
                structure.phase = Phase::Body;
                structure.count = 0;
            }
            return Ok(Some(end));
        }
        let invalid = match token {
            "fi" => state.structures.last().is_none_or(|structure| {
                structure.kind != StructureKind::If || structure.phase != Phase::Body || structure.count == 0
            }),
            "done" => state.structures.last().is_none_or(|structure| {
                !matches!(
                    structure.kind,
                    StructureKind::For | StructureKind::While | StructureKind::Until | StructureKind::Repeat
                ) || structure.phase != Phase::Body
                    || structure.count == 0
            }),
            "esac" => state
                .structures
                .last()
                .is_none_or(|structure| structure.kind != StructureKind::Case || structure.phase == Phase::Header),
            _ => false,
        };
        if invalid {
            return Err(OpaqueReason::CompoundCommand);
        }
        state.structures.pop();
        bash_statement(state);
        state.compound_end = true;
        return Ok(Some(end));
    }
    if !at_command_start(state) {
        return Ok(None);
    }
    let definition_length = bash_function_head_length(input, index);
    if definition_length > 0 {
        return Ok(Some(index + definition_length - 1));
    }
    let compound = matches!(token.as_deref(), Some("if" | "while" | "until" | "for" | "select" | "case"))
        || (token.as_deref() == Some("repeat") && state.context.dialect != Dialect::Bash);
    if compound {
        if depth + state.structures.len() >= MAX_SUBSTITUTION_DEPTH {
            return Err(OpaqueReason::CompoundCommand);
        }
        let token = token.as_deref().unwrap_or("");
        let kind = if token == "select" { StructureKind::For } else { structure_kind(token) };
        let phase =
            if matches!(token, "for" | "select" | "case" | "repeat") { Phase::Header } else { Phase::Condition };
        state.structures.push(Structure::new(kind, phase));
        state.dangling = false;
        return Ok(Some(end));
    }
    let prefix = match token.as_deref() {
        Some("coproc") => Some(&*COPROC_AHEAD_RE),
        Some("time") => Some(&*TIME_AHEAD_RE),
        _ => None,
    };
    if let Some(prefix) = prefix
        && let Some(matched) = prefix.matched_at(input, index)
    {
        return Ok(Some(index + unit_len(&matched) - 1));
    }
    Ok(None)
}

fn structure_kind(token: &str) -> StructureKind {
    match token {
        "if" => StructureKind::If,
        "while" => StructureKind::While,
        "until" => StructureKind::Until,
        "case" => StructureKind::Case,
        "repeat" => StructureKind::Repeat,
        _ => StructureKind::For,
    }
}

pub(crate) fn bash_function_head_length(input: &[u16], start: usize) -> usize {
    let Some(matched) = FUNCTION_HEAD_RE.search.find_from_utf16(input, start).next() else {
        return 0;
    };
    if matched.range.start != start {
        return 0;
    }
    let name = matched.group(2).map(|range| units(input, range.start, range.end));
    let valid = if matched.group(1).is_some() {
        name.is_some()
    } else {
        matched.group(3).is_some() && !name.as_deref().is_some_and(|name| BASH_NON_FUNCTION_KEYWORDS.contains(name))
    };
    if !valid {
        return 0;
    }
    let head_length = matched.range.end - matched.range.start;
    if FUNCTION_BODY_RE.test_at(input, matched.range.end) {
        head_length.saturating_sub(matched.group(4).map(|range| range.end - range.start).unwrap_or(0))
    } else {
        0
    }
}
