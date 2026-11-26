use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum PowerShellClause {
    Member,
    Block,
    If,
    Try,
    Clause,
    Switch,
    Paren,
    Do,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum After {
    Do,
    If,
    Try,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Indexable {
    None,
    Variable,
    Closed,
    Member,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hash {
    None,
    Expression,
    Clause,
    Switch,
}

static POWERSHELL_STOP_PARSING_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::new(r"--%(?=$|[\s;|&(){}])"));
static POWERSHELL_PARAMETER_COLON_RE: LazyLock<Pattern> =
    LazyLock::new(|| Pattern::with_flags(r"^-[\p{L}\p{Nl}_?][\p{L}\p{Nl}\d_?-]*:$", "u"));
static POWERSHELL_MERGE_REDIRECT_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::new(r"\d>&"));
static POWERSHELL_LABEL_RE: LazyLock<Pattern> = LazyLock::new(|| {
    Pattern::with_flags(r":[A-Za-z_]\w*(?=\s*(?:while|for|foreach|do|switch)(?=$|[\s&(),;{|}]))", "i")
});
static POWERSHELL_KEYWORD_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::new(r"[A-Za-z]+(?=$|[\s&(),;{|}])"));
static POWERSHELL_FOREACH_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::with_flags(r"foreach\s*\(", "i"));
static POWERSHELL_PARAM_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::with_flags(r"param\s*\(", "i"));
static POWERSHELL_IN_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::with_flags(r"in(?=$|[\s&(),;{|}])", "i"));
static POWERSHELL_AT_OPENER_RE: LazyLock<Pattern> =
    LazyLock::new(|| Pattern::with_flags(r#"[({'"\p{L}\p{Nl}\d_?]"#, "u"));
static POWERSHELL_INDEXABLE_VAR_RE: LazyLock<Pattern> =
    LazyLock::new(|| Pattern::with_flags(r"(?::[\p{L}\p{Nl}\d_?]+|[\p{L}\p{Nl}\d_][\p{L}\p{Nl}\d_?]*)", "u"));
static POWERSHELL_HERE_HEADER_RE: LazyLock<Pattern> =
    LazyLock::new(|| Pattern::new(r#"@['"][^\S\r\n]*(?:\r\n|\r|\n)"#));
static POWERSHELL_UNICODE_ESCAPE_RE: LazyLock<Pattern> =
    LazyLock::new(|| Pattern::with_flags(r"u\{([0-9a-f]{1,6})\}", "i"));
static POWERSHELL_EXPRESSION_RE: LazyLock<Pattern> = LazyLock::new(|| {
    Pattern::with_flags(
        r"(?:!*(?:\+\+|--|[+-])?\$(?:\{|(?:\?|\$|\^)(?=$|[\s;|&(){},#>])|:[\p{L}\p{Nl}\d_?]+|[\p{L}\p{Nl}\d_][\p{L}\p{Nl}\d_?]*(?::[\p{L}\p{Nl}\d_?]+)?)|!(?![\p{L}\p{Nl}_.\d])|\+(?![=\p{L}\p{Nl}_.\d])|--(?!=)|-(?![=\p{L}\p{Nl}\d_?.])|,(?!\s*$)|!*[+-]?(?:0[xX][\da-fA-F]+|0[bB][01]+|(?:\d+(?:\.(?!\.)\d*|(?!\.(?!\.)))|\.\d+)(?:[eE][+-]?\d+)?)(?:[dDlLnN]|[uU][sSyYlL]?|[sSyY])?(?:[kKmMgGtTpP][bB])?(?=$|[\s!#%&()*+,\-/.;<=>\]|{}])|-(?:not|bnot|join|[ic]?split)(?![\p{L}\p{Nl}]))",
        "iu",
    )
});
static POWERSHELL_BRACKET_WORD_RE: LazyLock<Pattern> =
    LazyLock::new(|| Pattern::with_flags(r"^[\p{L}\p{Nl}_][\p{L}\p{Nl}\d_.#+`\\]*$", "u"));
static POWERSHELL_IDENT_CHAR_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::with_flags(r"^[\p{L}\p{Nl}\d_]$", "u"));
static POWERSHELL_INDEXABLE_CHAR_RE: LazyLock<Pattern> =
    LazyLock::new(|| Pattern::with_flags(r"^[\p{L}\p{Nl}\d_:?]$", "u"));
static POWERSHELL_REDIRECT_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::new(r"^(?:(?:[1-6]|\*)?>>?|[2-6*]>&1)$"));

const POWERSHELL_ESCAPES: &[(u16, &str)] = &[
    (b'0' as u16, "\0"),
    (b'a' as u16, "\x07"),
    (b'b' as u16, "\x08"),
    (b'e' as u16, "\x1b"),
    (b'f' as u16, "\x0c"),
    (b'n' as u16, "\x0a"),
    (b'r' as u16, "\x0d"),
    (b't' as u16, "\x09"),
    (b'v' as u16, "\x0b"),
];

struct Escape {
    value: Vec<u16>,
    end: usize,
}

struct SubExpression {
    skipped: HashSet<usize>,
    raw_end: Option<usize>,
}

enum HereString {
    Scanned { commands: Vec<Command>, source: String, end: usize },
    Opaque(OpaqueReason),
}

pub fn scan_power_shell(input: &str) -> ScanResult {
    let units: Vec<u16> = input.encode_utf16().collect();
    if units.len() > MAX_INPUT_LENGTH {
        return ScanResult::Opaque { reason: OpaqueReason::InvalidStructure };
    }
    if units.iter().any(|unit| matches!(*unit, 0x0000 | 0x0085 | 0x2013..=0x2015 | 0x2018..=0x201e | 0xfeff)) {
        return ScanResult::Opaque { reason: OpaqueReason::InvalidStructure };
    }
    let mut budget = (MAX_INPUT_LENGTH * MAX_SUBSTITUTION_DEPTH) as i64;
    match scan_power_shell_list(&units, 0, 0, &mut budget, None, Hash::None, None) {
        BashResult::Opaque(reason) => ScanResult::Opaque { reason },
        BashResult::Scanned { commands, .. } => ScanResult::Scanned { commands },
    }
}

fn scan_power_shell_list(
    input: &[u16],
    start: usize,
    depth: usize,
    budget: &mut i64,
    close: Option<u16>,
    hash: Hash,
    skipped: Option<&HashSet<usize>>,
) -> BashResult {
    if depth >= MAX_SUBSTITUTION_DEPTH || *budget < 0 {
        return BashResult::Opaque(OpaqueReason::InvalidStructure);
    }
    let mut state = PowerShellList::new(input, start, hash);
    let mut index = start;
    while index < input.len() {
        *budget -= 1;
        if *budget < 0 {
            return BashResult::Opaque(OpaqueReason::InvalidStructure);
        }
        if skipped.is_some_and(|skipped| skipped.contains(&index)) {
            index += 1;
            continue;
        }
        let char = input[index];
        let in_expression = state.expression && !state.redirect_target;
        let at_boundary = !state.started || (!in_expression && state.word.ends_with(&[b',' as u16]));
        if !state.started {
            state.word_start = index;
        }
        if state.stop_parsing {
            let stop = power_shell_stop_parsing(input, index, skipped);
            let text = trim_units(input, index, stop);
            if !text.is_empty() {
                let mut lead = index;
                while lead < stop && is_js_space(input[lead]) {
                    lead += 1;
                }
                state.word_start = lead;
                state.word = encode(&text);
                state.started = true;
                state.finish_word(lead + unit_len(&text));
            }
            state.stop_parsing = false;
            index = stop;
            continue;
        }
        if let Some(quote) = state.quote {
            let mut next_quote = index + 1;
            while skipped.is_some_and(|skipped| skipped.contains(&next_quote)) {
                next_quote += 1;
            }
            let next = input.get(next_quote).copied();
            if char == quote && next == Some(quote) {
                state.word.push(quote);
                index = next_quote + 1;
                continue;
            }
            if char == quote {
                state.quote = None;
                if state.standalone {
                    state.finish_word(index + 1);
                    state.indexable = Indexable::Closed;
                }
                index += 1;
                continue;
            }
            if quote == b'"' as u16 && char == b'`' as u16 {
                let Some(escape) = power_shell_escape(input, index, false, skipped) else {
                    return BashResult::Opaque(OpaqueReason::UnterminatedEscape);
                };
                state.word.extend_from_slice(&escape.value);
                index = escape.end + 1;
                continue;
            }
            if quote == b'"' as u16 && char == b'$' as u16 && input.get(index + 1) == Some(&(b'{' as u16)) {
                let Some(end) = power_shell_braced_variable(input, index, skipped) else {
                    return BashResult::Opaque(OpaqueReason::InvalidStructure);
                };
                state.word.extend_from_slice(&input[index..=end]);
                index = end + 1;
                continue;
            }
            if quote == b'"' as u16 && char == b'$' as u16 && input.get(index + 1) == Some(&(b'(' as u16)) {
                let Some(sub) = power_shell_sub_expression(input, index + 2, false, budget, skipped) else {
                    return BashResult::Opaque(OpaqueReason::InvalidStructure);
                };
                let result = scan_power_shell_list(
                    input,
                    index + 2,
                    depth + 1,
                    budget,
                    Some(b')' as u16),
                    Hash::None,
                    Some(&sub.skipped),
                );
                let (commands, end) = match result {
                    BashResult::Opaque(reason) => return BashResult::Opaque(reason),
                    BashResult::Scanned { commands, end } => (commands, end),
                };
                state.nested_commands.extend(commands);
                let end = sub.raw_end.filter(|raw| *raw > end).unwrap_or(end);
                state.word.extend_from_slice(&input[index..=end]);
                index = end + 1;
                continue;
            }
            state.word.push(char);
            index += 1;
            continue;
        }
        if char == b'\'' as u16 || char == b'"' as u16 {
            if !state.started && state.words.is_empty() && !state.invocation {
                state.expression = true;
            }
            state.quote = Some(char);
            let dynamic_member = state.after_member_operator();
            state.standalone = at_boundary || (state.expression && !state.redirect_target) || dynamic_member;
            if dynamic_member {
                state.has_member_access = true;
            } else if state.standalone {
                state.member_chain_start = Some(index);
                state.has_member_access = false;
            } else {
                state.reset_member();
            }
            state.started = true;
            index += 1;
            continue;
        }
        if char == b'`' as u16 {
            let Some(escape) = power_shell_escape(input, index, at_boundary || in_expression, skipped) else {
                return BashResult::Opaque(OpaqueReason::UnterminatedEscape);
            };
            let raw_space = input.get(index + 1).is_some_and(|unit| is_js_space(*unit));
            if (in_expression || at_boundary) && state.started && raw_space {
                state.finish_word(index);
            } else if !at_boundary || !raw_space {
                state.started = true;
                let value = String::from_utf16_lossy(&escape.value);
                if !(state.indexable == Indexable::Member && POWERSHELL_IDENT_CHAR_RE.is_match_str(&value)) {
                    state.reset_member();
                }
                state.word.extend_from_slice(&escape.value);
            }
            index = escape.end + 1;
            continue;
        }
        if !state.started
            && !state.words.is_empty()
            && !state.expression
            && state.clause.is_none()
            && !state.redirect_target
            && POWERSHELL_STOP_PARSING_RE.test_at(input, index)
        {
            state.word = encode("--%");
            state.started = true;
            state.finish_word(index + 3);
            index += 3;
            continue;
        }
        if char == b'<' as u16
            && input.get(index + 1) == Some(&(b'#' as u16))
            && (at_boundary || in_expression || state.indexable == Indexable::Member)
        {
            let member_operator = state.after_member_operator();
            if state.started {
                state.finish_word(index);
            }
            let Some(end) = index_of(input, index + 2, "#>") else {
                return BashResult::Opaque(OpaqueReason::InvalidStructure);
            };
            if !member_operator {
                state.reset_member();
            }
            index = end + 2;
            continue;
        }
        if char == b'#' as u16
            && (at_boundary
                || state.indexable == Indexable::Member
                || (in_expression
                    && !(close == Some(b']' as u16)
                        && POWERSHELL_BRACKET_WORD_RE.is_match_str(&String::from_utf16_lossy(&state.word)))))
        {
            let prev = state.previous_word();
            let member_operator = state.after_member_operator();
            if (state.clause.is_some() && state.clause != Some(PowerShellClause::Member))
                || prev.ends_with(&[b',' as u16])
                || member_operator
            {
                state.finish_word(index);
            } else if !state.words.is_empty() || state.started || state.invocation {
                state.finish_command(index, false);
            }
            state.statement_head = !state.dangling;
            let newline = [index_of(input, index, "\n"), index_of(input, index, "\r")].into_iter().flatten().min();
            let Some(newline) = newline else {
                break;
            };
            index = if input[newline] == b'\r' as u16 && input.get(newline + 1) == Some(&(b'\n' as u16)) {
                newline + 2
            } else {
                newline + 1
            };
            continue;
        }
        if in_expression
            && state.started
            && (char == b'>' as u16 || (char == b'*' as u16 && input.get(index + 1) == Some(&(b'>' as u16))))
        {
            state.finish_word(index);
        }
        let redirect = if !state.started
            && (char == b'>' as u16
                || char == b'*' as u16
                || (is_digit(char) && (!state.words.is_empty() || POWERSHELL_MERGE_REDIRECT_RE.test_at(input, index))))
        {
            match power_shell_redirect(input, index) {
                Ok(redirect) => redirect,
                Err(reason) => return BashResult::Opaque(reason),
            }
        } else {
            None
        };
        if let Some(redirect) = redirect {
            if state.redirect_target
                || state.words.is_empty()
                || (state.clause.is_some() && state.clause != Some(PowerShellClause::Member))
            {
                return BashResult::Opaque(OpaqueReason::InvalidRedirect);
            }
            state.redirect_target = !redirect.contains('&');
            if state.redirect_target {
                state.has_file_redirect = true;
            }
            state.reset_member();
            index += unit_len(&redirect);
            state.command_end = index;
            continue;
        }
        if !state.started
            && state.words.is_empty()
            && !state.invocation
            && !state.expression
            && state.clause.is_none()
            && char == b':' as u16
            && let Some(label) = POWERSHELL_LABEL_RE.at(input, index)
        {
            index += label.end - label.start;
            continue;
        }
        if !state.started
            && !state.invocation
            && state.clause.is_none()
            && (if state.words.is_empty() {
                !state.expression
            } else {
                state.words.iter().all(|word| word.starts_with('['))
            })
            && matches!(char, 0x41..=0x5a | 0x61..=0x7a)
        {
            let keyword = POWERSHELL_KEYWORD_RE.matched_at(input, index).map(|keyword| keyword.to_lowercase());
            if let Some(keyword) = keyword {
                let keyword_length = keyword.len();
                let matched = if !state.words.is_empty() {
                    (keyword == "param" && POWERSHELL_PARAM_RE.test_at(input, index))
                        || keyword == "class"
                        || keyword == "enum"
                } else {
                    (keyword == "param" && POWERSHELL_PARAM_RE.test_at(input, index))
                        || (keyword == "foreach" && POWERSHELL_FOREACH_RE.test_at(input, index))
                        || (keyword == "until" && state.after == Some(After::Do))
                        || ((keyword == "else" || keyword == "elseif") && state.after == Some(After::If))
                        || ((keyword == "catch" || keyword == "finally") && state.after == Some(After::Try))
                        || matches!(
                            keyword.as_str(),
                            "if" | "for"
                                | "while"
                                | "do"
                                | "switch"
                                | "function"
                                | "filter"
                                | "workflow"
                                | "parallel"
                                | "sequence"
                                | "configuration"
                                | "try"
                                | "begin"
                                | "process"
                                | "end"
                                | "clean"
                                | "trap"
                                | "class"
                                | "enum"
                                | "data"
                                | "dynamicparam"
                                | "using"
                                | "break"
                                | "continue"
                        )
                };
                if matched {
                    state.expression = matches!(keyword.as_str(), "class" | "enum" | "break" | "continue");
                    state.clause = Some(if keyword == "using" || keyword == "break" || keyword == "continue" {
                        PowerShellClause::Member
                    } else if keyword == "param"
                        || keyword == "until"
                        || (keyword == "while" && state.after == Some(After::Do))
                    {
                        PowerShellClause::Paren
                    } else if keyword == "do" {
                        PowerShellClause::Do
                    } else if keyword == "if" || keyword == "elseif" {
                        PowerShellClause::If
                    } else if keyword == "try" || keyword == "catch" {
                        PowerShellClause::Try
                    } else if keyword == "switch" {
                        PowerShellClause::Switch
                    } else if keyword == "class" || keyword == "enum" {
                        PowerShellClause::Clause
                    } else {
                        PowerShellClause::Block
                    });
                    state.after = None;
                    index += keyword_length;
                    continue;
                }
                if state.words.is_empty() && matches!(keyword.as_str(), "return" | "throw" | "exit") {
                    state.after = None;
                    index += keyword_length;
                    continue;
                }
            }
        }
        let assign = in_expression
            && state.clause.is_none()
            && (char == b'=' as u16
                || (matches!(char, 0x2d | 0x2b | 0x2a | 0x2f | 0x25) && input.get(index + 1) == Some(&(b'=' as u16))));
        if assign
            || (in_expression && close == Some(b')' as u16) && !state.started && POWERSHELL_IN_RE.test_at(input, index))
        {
            if assign {
                state.finish_word(index);
            }
            state.words.clear();
            state.raw_words.clear();
            state.word_ends.clear();
            state.expression = false;
            state.has_file_redirect = false;
            state.reset_member();
            index += if !assign || char != b'=' as u16 { 2 } else { 1 };
            continue;
        }
        if at_boundary && char == b'@' as u16 && !POWERSHELL_AT_OPENER_RE.test_at(input, index + 1) {
            return BashResult::Opaque(OpaqueReason::InvalidStructure);
        }
        if at_boundary
            && char == b'@' as u16
            && (input.get(index + 1) == Some(&(b'\'' as u16)) || input.get(index + 1) == Some(&(b'"' as u16)))
        {
            if state.started {
                state.finish_word(index);
            }
            state.word_start = index;
            let literal = power_shell_here_string(input, index, depth, budget, skipped);
            let (commands, source, end) = match literal {
                HereString::Opaque(reason) => return BashResult::Opaque(reason),
                HereString::Scanned { commands, source, end } => (commands, source, end),
            };
            if state.words.is_empty() && !state.invocation {
                state.expression = true;
            }
            state.nested_commands.extend(commands);
            state.word = encode(&source);
            state.started = true;
            state.finish_word(end + 1);
            state.member_chain_start = Some(state.word_start);
            state.has_member_access = false;
            state.indexable = Indexable::Closed;
            index = end + 1;
            continue;
        }
        if char == b'$' as u16 && input.get(index + 1) == Some(&(b'{' as u16)) {
            let Some(end) = power_shell_braced_variable(input, index, skipped) else {
                return BashResult::Opaque(OpaqueReason::InvalidStructure);
            };
            if !state.started && state.words.is_empty() && !state.invocation && hash != Hash::Switch {
                state.expression = true;
            }
            let dynamic_member = state.after_member_operator();
            if dynamic_member {
                state.indexable = Indexable::Closed;
                state.has_member_access = true;
            } else if at_boundary {
                state.member_chain_start = Some(index);
                state.has_member_access = false;
                state.indexable = Indexable::Closed;
            } else {
                state.reset_member();
            }
            state.word.extend_from_slice(&input[index..=end]);
            state.started = true;
            index = end + 1;
            if state.expression && !state.redirect_target && input.get(index) != Some(&(b'[' as u16)) {
                state.finish_word(index);
            }
            continue;
        }
        let opener = if ((char == b'$' as u16 || (at_boundary && char == b'@' as u16))
            && input.get(index + 1) == Some(&(b'(' as u16)))
            || (at_boundary && char == b'@' as u16 && input.get(index + 1) == Some(&(b'{' as u16)))
        {
            Some(index + 1)
        } else if char == b'(' as u16
            || char == b'{' as u16
            || (char == b'[' as u16
                && (in_expression
                    || state.indexable != Indexable::None
                    || (!state.started
                        && !state.invocation
                        && (state.words.is_empty()
                            || state.clause == Some(PowerShellClause::Block)
                            || state.clause == Some(PowerShellClause::Try)))))
        {
            Some(index)
        } else {
            None
        };
        if let Some(opener) = opener {
            let token_index = index;
            let open_char = input[opener];
            let dynamic_member_opener = state.after_member_operator();
            let prior_indexable = state.indexable != Indexable::None;
            let is_method_call = char == b'(' as u16
                && opener == token_index
                && state.member_chain_start.is_some()
                && ((state.indexable == Indexable::Member
                    && state.started
                    && !state.word.last().is_some_and(|unit| matches!(*unit, 0x2e | 0x3a)))
                    || (state.indexable == Indexable::Closed && !state.started && state.has_member_access));
            let standalone_block = at_boundary || in_expression || char == b'(' as u16 || open_char == b'{' as u16;
            let block_close = Some(if open_char == b'(' as u16 {
                b')' as u16
            } else if open_char == b'[' as u16 {
                b']' as u16
            } else {
                b'}' as u16
            });
            let block_hash = if open_char == b'[' as u16 || (char == b'@' as u16 && open_char == b'{' as u16) {
                Hash::Expression
            } else if state.clause == Some(PowerShellClause::Switch) && open_char == b'{' as u16 {
                Hash::Switch
            } else if (state.clause == Some(PowerShellClause::Clause) && open_char == b'{' as u16)
                || (close == Some(b']' as u16) && char == b'(' as u16 && state.started)
            {
                Hash::Clause
            } else {
                Hash::None
            };
            if state.started && (char == b'{' as u16 || char == b'(' as u16 || (char == b'@' as u16 && at_boundary)) {
                state.finish_word(index);
                if state.stop_parsing {
                    continue;
                }
                state.word_start = index;
            }
            if !state.started && state.words.is_empty() && !state.invocation {
                state.expression = true;
            }
            let result = scan_power_shell_list(input, opener + 1, depth + 1, budget, block_close, block_hash, skipped);
            let (commands, end) = match result {
                BashResult::Opaque(reason) => return BashResult::Opaque(reason),
                BashResult::Scanned { commands, end } => (commands, end),
            };
            state.nested_commands.extend(commands);
            state.started = true;
            state.word.extend_from_slice(&input[index..=end]);
            index = end;
            if is_method_call && let Some(chain_start) = state.member_chain_start {
                state.finish_word(index + 1);
                let call_resource = units(input, chain_start, end + 1);
                let head = units(input, chain_start, opener);
                let args = units(input, opener, end + 1);
                state.nested_commands.push(Command {
                    resource: call_resource.clone(),
                    words: vec![head.clone(), args.clone()],
                    raw_words: vec![head.clone(), args.clone()],
                    word_ends: Some(vec![unit_len(&head), unit_len(&call_resource)]),
                    statement_head: None,
                    declaration: None,
                    redirect_word_count: None,
                    redirect_prefix_end: None,
                });
                state.indexable = Indexable::Closed;
                state.has_member_access = false;
            } else if open_char == b'[' as u16 {
                state.indexable = Indexable::Closed;
                if !prior_indexable {
                    state.member_chain_start = Some(token_index);
                }
                if input.get(index + 1) != Some(&(b'[' as u16)) {
                    state.finish_word(index + 1);
                }
            } else if standalone_block {
                state.finish_word(index + 1);
                state.indexable = Indexable::Closed;
                if !dynamic_member_opener {
                    state.member_chain_start = Some(token_index);
                }
                state.has_member_access = dynamic_member_opener;
            }
            if (state.clause == Some(PowerShellClause::Paren) && char == b'(' as u16)
                || (state.clause.is_some() && state.clause != Some(PowerShellClause::Paren) && char == b'{' as u16)
            {
                let completed = match state.clause {
                    Some(PowerShellClause::Do) => Some(After::Do),
                    Some(PowerShellClause::If) => Some(After::If),
                    Some(PowerShellClause::Try) => Some(After::Try),
                    _ => None,
                };
                state.clause = Some(PowerShellClause::Member);
                state.finish_command(index + 1, false);
                state.after = completed;
            }
            index += 1;
            continue;
        }
        if close == Some(char) {
            break;
        }
        if char == b'}' as u16 || char == b')' as u16 || (char == b']' as u16 && in_expression) {
            return BashResult::Opaque(OpaqueReason::InvalidStructure);
        }
        if !state.started
            && state.words.is_empty()
            && ((char == b'&' as u16 && input.get(index + 1) != Some(&(b'&' as u16)))
                || (char == b'.' as u16
                    && (input.get(index + 1).is_none()
                        || input.get(index + 1).is_some_and(|next| {
                            is_js_space(*next)
                                || matches!(
                                    *next,
                                    0x22 | 0x27 | 0x24 | 0x28 | 0x29 | 0x2c | 0x3b | 0x7b | 0x7c | 0x7d | 0x26
                                )
                        }))))
        {
            if state.invocation {
                return BashResult::Opaque(OpaqueReason::InvalidStructure);
            }
            state.segment = index;
            state.invocation = true;
            state.after = None;
            index += 1;
            continue;
        }
        if is_js_space(char) && char != b'\n' as u16 && char != b'\r' as u16 {
            let member_operator = state.after_member_operator();
            state.finish_word(index);
            if !member_operator {
                state.reset_member();
            }
            index += 1;
            continue;
        }
        let next = input.get(index + 1).copied();
        let separator: Option<&'static str> = if char == b'\r' as u16 && next == Some(b'\n' as u16) {
            Some("\r\n")
        } else if (char == b'&' as u16 && next == Some(b'&' as u16))
            || (char == b'|' as u16 && next == Some(b'|' as u16))
        {
            Some(if char == b'&' as u16 { "&&" } else { "||" })
        } else if char == b';' as u16 {
            Some(";")
        } else if char == b'|' as u16 {
            Some("|")
        } else if char == b'&' as u16 {
            Some("&")
        } else if char == b'\n' as u16 {
            Some("\n")
        } else if char == b'\r' as u16 {
            Some("\r")
        } else {
            None
        };
        if let Some(separator) = separator {
            if close == Some(b']' as u16) && matches!(separator, ";" | "|" | "||" | "&&" | "&") {
                return BashResult::Opaque(OpaqueReason::InvalidStructure);
            }
            if matches!(separator, "\n" | "\r" | "\r\n") {
                let prev = state.previous_word();
                let member_operator = state.after_member_operator();
                if (state.clause.is_some() && state.clause != Some(PowerShellClause::Member))
                    || prev.ends_with(&[b',' as u16])
                    || member_operator
                {
                    state.finish_word(index);
                    index += separator.len();
                    if !member_operator {
                        state.reset_member();
                    }
                    continue;
                }
                if !state.started && state.words.is_empty() && !state.invocation {
                    index += separator.len();
                    state.reset_member();
                    continue;
                }
            }
            let required = state.dangling || !matches!(separator, ";" | "\n" | "\r" | "\r\n");
            state.finish_command(index, required);
            state.dangling = !matches!(separator, ";" | "&" | "\n" | "\r" | "\r\n");
            state.statement_head = !state.dangling;
            index += separator.len();
            continue;
        }
        if !state.started
            && state.words.is_empty()
            && !state.invocation
            && hash != Hash::Switch
            && POWERSHELL_EXPRESSION_RE.test_at(input, index)
        {
            state.expression = true;
        }
        if (at_boundary || state.after_member_operator()) && char == b'$' as u16 {
            if POWERSHELL_INDEXABLE_VAR_RE.test_at(input, index + 1) {
                if state.after_member_operator() {
                    state.has_member_access = true;
                } else {
                    state.member_chain_start = Some(index);
                    state.has_member_access = false;
                }
                state.indexable = Indexable::Variable;
            } else {
                state.reset_member();
            }
        } else if state.indexable == Indexable::Closed {
            let next_space = input.get(index + 1).is_some_and(|next| is_js_space(*next));
            let after_space = input.get(index + 2).is_some_and(|next| is_js_space(*next));
            state.indexable = if (char == b'.' as u16 && next != Some(b'.' as u16) && (!next_space || in_expression))
                || (!state.started && char == b':' as u16 && next == Some(b':' as u16))
                || (!state.started
                    && char == b'?' as u16
                    && next == Some(b'.' as u16)
                    && input.get(index + 2) != Some(&(b'.' as u16))
                    && (!after_space || in_expression))
            {
                Indexable::Member
            } else if !state.started && char == b'?' as u16 && next == Some(b'[' as u16) {
                Indexable::Closed
            } else {
                Indexable::None
            };
            if state.indexable == Indexable::Member {
                state.has_member_access = true;
            } else if state.indexable == Indexable::None {
                state.reset_member();
            }
        } else if state.indexable == Indexable::Variable {
            let next_space = input.get(index + 1).is_some_and(|next| is_js_space(*next));
            state.indexable = if (char == b'.' as u16 && next != Some(b'.' as u16) && (!next_space || in_expression))
                || (char == b':' as u16 && next == Some(b':' as u16))
            {
                Indexable::Member
            } else if (char == b'.' as u16 && next == Some(b'.' as u16))
                || !POWERSHELL_INDEXABLE_CHAR_RE.is_match_str(&char_text(char))
            {
                Indexable::None
            } else {
                Indexable::Variable
            };
            if state.indexable == Indexable::Member {
                state.has_member_access = true;
            } else if state.indexable == Indexable::None {
                state.reset_member();
            }
        } else if state.indexable == Indexable::Member {
            let next_space = input.get(index + 1).is_some_and(|next| is_js_space(*next));
            state.indexable = if char == b'.' as u16 && next == Some(b'.' as u16) {
                Indexable::None
            } else if (char == b'.' as u16 && (!next_space || in_expression))
                || (char == b':' as u16
                    && (next == Some(b':' as u16) || (index > 0 && input.get(index - 1) == Some(&(b':' as u16)))))
                || (char == b'?' as u16 && next == Some(b'.' as u16) && input.get(index + 2) != Some(&(b'.' as u16)))
                || POWERSHELL_IDENT_CHAR_RE.is_match_str(&char_text(char))
            {
                Indexable::Member
            } else {
                Indexable::None
            };
            if state.indexable == Indexable::None {
                state.reset_member();
            }
        }
        state.after = None;
        state.started = true;
        state.dangling = false;
        state.word.push(char);
        index += 1;
    }

    if close.is_some() && input.get(index).copied() != close {
        return BashResult::Opaque(OpaqueReason::InvalidStructure);
    }
    if state.quote.is_some() {
        return BashResult::Opaque(OpaqueReason::UnterminatedQuote);
    }
    state.finish_command(index, false);
    if state.redirect_target || state.invalid || state.dangling {
        return BashResult::Opaque(OpaqueReason::InvalidStructure);
    }
    if state.commands.iter().any(|command| command.words.first().is_none_or(|word| word.is_empty())) {
        return BashResult::Opaque(OpaqueReason::DynamicCommandName);
    }
    BashResult::Scanned { commands: state.commands, end: index }
}

struct PowerShellList<'a> {
    input: &'a [u16],
    commands: Vec<Command>,
    nested_commands: Vec<Command>,
    words: Vec<String>,
    raw_words: Vec<String>,
    word_ends: Vec<usize>,
    initial_expression: bool,
    initial_clause: Option<PowerShellClause>,
    segment: usize,
    word: Vec<u16>,
    started: bool,
    word_start: usize,
    quote: Option<u16>,
    standalone: bool,
    expression: bool,
    clause: Option<PowerShellClause>,
    after: Option<After>,
    indexable: Indexable,
    member_chain_start: Option<usize>,
    has_member_access: bool,
    has_file_redirect: bool,
    stop_parsing: bool,
    command_end: usize,
    statement_head: bool,
    invalid: bool,
    redirect_target: bool,
    dangling: bool,
    invocation: bool,
}

impl<'a> PowerShellList<'a> {
    fn new(input: &'a [u16], start: usize, hash: Hash) -> Self {
        let initial_expression = matches!(hash, Hash::Expression | Hash::Clause);
        let initial_clause = matches!(hash, Hash::Clause | Hash::Switch).then_some(PowerShellClause::Member);
        Self {
            input,
            commands: Vec::new(),
            nested_commands: Vec::new(),
            words: Vec::new(),
            raw_words: Vec::new(),
            word_ends: Vec::new(),
            initial_expression,
            initial_clause,
            segment: start,
            word: Vec::new(),
            started: false,
            word_start: start,
            quote: None,
            standalone: false,
            expression: initial_expression,
            clause: initial_clause,
            after: None,
            indexable: Indexable::None,
            member_chain_start: None,
            has_member_access: false,
            has_file_redirect: false,
            stop_parsing: false,
            command_end: start,
            statement_head: true,
            invalid: false,
            redirect_target: false,
            dangling: false,
            invocation: false,
        }
    }

    fn previous_word(&self) -> Vec<u16> {
        if self.started {
            return self.word.clone();
        }
        self.words.last().map(|word| encode(word)).unwrap_or_default()
    }

    fn after_member_operator(&self) -> bool {
        self.indexable == Indexable::Member
            && self.previous_word().last().is_some_and(|unit| matches!(*unit, 0x3a | 0x2e))
    }

    fn reset_member(&mut self) {
        self.indexable = Indexable::None;
        self.member_chain_start = None;
        self.has_member_access = false;
    }

    fn finish_word(&mut self, end: usize) {
        if !self.started {
            return;
        }
        if self.word == encode("--%")
            && !self.words.is_empty()
            && !self.expression
            && self.clause.is_none()
            && !self.redirect_target
            && !self.input.get(self.word_start).is_some_and(|unit| matches!(*unit, 0x27 | 0x22))
            && !self.raw_words.last().is_some_and(|word| word.ends_with(','))
            && !self.raw_words.last().is_some_and(|word| POWERSHELL_PARAMETER_COLON_RE.is_match_str(word))
        {
            self.stop_parsing = true;
        }
        if !self.redirect_target {
            if self.words.is_empty() && !self.invocation {
                self.segment = self.word_start;
            }
            self.words.push(String::from_utf16_lossy(&self.word));
            self.raw_words.push(units(self.input, self.word_start, end));
            self.word_ends.push(end);
            self.dangling = false;
        }
        self.command_end = end;
        self.redirect_target = false;
        self.word.clear();
        self.started = false;
    }

    fn finish_command(&mut self, end: usize, required: bool) {
        self.finish_word(end);
        let statement_head = self.statement_head && !self.invocation && !self.expression && self.clause.is_none();
        if !self.words.is_empty() && ((!self.expression && self.clause.is_none()) || self.has_file_redirect) {
            self.commands.push(Command {
                resource: trim_end_units(self.input, self.segment, self.command_end),
                words: self.words.clone(),
                raw_words: self.raw_words.clone(),
                word_ends: Some(self.word_ends.iter().map(|value| value - self.segment).collect()),
                statement_head: statement_head.then_some(true),
                declaration: None,
                redirect_word_count: None,
                redirect_prefix_end: None,
            });
        } else if (!self.expression && self.clause.is_none() && self.invocation)
            || (required && self.words.is_empty() && self.nested_commands.is_empty())
            || (self.clause.is_some() && self.clause != Some(PowerShellClause::Member))
        {
            self.invalid = true;
        }
        if self.redirect_target {
            self.invalid = true;
        }
        self.commands.append(&mut self.nested_commands);
        self.words.clear();
        self.raw_words.clear();
        self.word_ends.clear();
        self.redirect_target = false;
        self.has_file_redirect = false;
        self.invocation = false;
        self.expression = self.initial_expression;
        self.clause = self.initial_clause;
        self.after = None;
        self.reset_member();
        self.stop_parsing = false;
    }
}

fn trim_end_units(input: &[u16], start: usize, end: usize) -> String {
    let end = end.min(input.len());
    let mut to = end;
    while to > start && is_js_space(input[to - 1]) {
        to -= 1;
    }
    units(input, start, to)
}

fn is_digit(unit: u16) -> bool {
    matches!(unit, 0x30..=0x39)
}

fn char_text(unit: u16) -> String {
    char::from_u32(u32::from(unit)).map(|char| char.to_string()).unwrap_or_default()
}

fn push_code_point(word: &mut Vec<u16>, code: u32) {
    if code <= 0xffff {
        word.push(code as u16);
        return;
    }
    let value = code - 0x10000;
    word.push(0xd800 + (value >> 10) as u16);
    word.push(0xdc00 + (value & 0x3ff) as u16);
}

fn power_shell_escape(
    input: &[u16],
    start: usize,
    line_continuation: bool,
    skipped: Option<&HashSet<usize>>,
) -> Option<Escape> {
    let mut next = start + 1;
    while skipped.is_some_and(|skipped| skipped.contains(&next)) {
        next += 1;
    }
    let char = *input.get(next)?;
    if char == b'\r' as u16 || char == b'\n' as u16 {
        let crlf = line_continuation && char == b'\r' as u16 && input.get(next + 1) == Some(&(b'\n' as u16));
        return Some(Escape {
            value: if crlf { encode("\r\n") } else { encode(&char_text(char)) },
            end: next + usize::from(crlf),
        });
    }
    if char == b'u' as u16 && input.get(next + 1) == Some(&(b'{' as u16)) {
        let range = POWERSHELL_UNICODE_ESCAPE_RE.at(input, next)?;
        let code = u32::from_str_radix(&units(input, next + 2, range.end - 1), 16).ok()?;
        if code > 0x10ffff {
            return None;
        }
        let mut value = Vec::new();
        push_code_point(&mut value, code);
        return Some(Escape { value, end: range.end - 1 });
    }
    let value = POWERSHELL_ESCAPES
        .iter()
        .find(|(key, _)| *key == char)
        .map(|(_, value)| encode(value))
        .unwrap_or_else(|| encode(&char_text(char)));
    Some(Escape { value, end: next })
}

fn power_shell_braced_variable(input: &[u16], start: usize, skipped: Option<&HashSet<usize>>) -> Option<usize> {
    let mut index = start + 2;
    while index < input.len() {
        if skipped.is_some_and(|skipped| skipped.contains(&index)) {
            index += 1;
            continue;
        }
        if input[index] == b'`' as u16 {
            let escape = power_shell_escape(input, index, false, skipped)?;
            index = escape.end + 1;
            continue;
        }
        if input[index] == b'}' as u16 {
            return (index > start + 2).then_some(index);
        }
        index += 1;
    }
    None
}

fn power_shell_sub_expression(
    input: &[u16],
    start: usize,
    here_string: bool,
    budget: &mut i64,
    skipped: Option<&HashSet<usize>>,
) -> Option<SubExpression> {
    let mut next_skipped = skipped.cloned().unwrap_or_default();
    let mut depth: i64 = 1;
    let mut raw_end = None;
    let mut index = start;
    while index < input.len() {
        *budget -= 1;
        if *budget < 0 {
            return None;
        }
        if next_skipped.contains(&index) {
            index += 1;
            continue;
        }
        let char = input[index];
        if char == b'(' as u16 {
            depth += 1;
        } else if char == b')' as u16 {
            depth -= 1;
            if depth == 0 && raw_end.is_none() {
                raw_end = Some(index);
            }
        } else if !here_string && (char == b'"' as u16 || char == b'`' as u16) {
            let mut next = index + 1;
            while next_skipped.contains(&next) {
                next += 1;
            }
            if input.get(next) == Some(&(b'"' as u16)) {
                next_skipped.insert(index);
                index = next;
            }
        }
        index += 1;
    }
    Some(SubExpression { skipped: next_skipped, raw_end })
}

fn power_shell_stop_parsing(input: &[u16], start: usize, skipped: Option<&HashSet<usize>>) -> usize {
    let mut quoted = false;
    let mut index = start;
    while index < input.len() {
        if skipped.is_some_and(|skipped| skipped.contains(&index)) {
            index += 1;
            continue;
        }
        if input[index] == b'"' as u16 {
            quoted = !quoted;
        }
        if input[index] == b'\r' as u16
            || input[index] == b'\n' as u16
            || ((input[index] == b'|' as u16
                || (input[index] == b'&' as u16 && input.get(index + 1) == Some(&(b'&' as u16))))
                && !quoted)
        {
            return index;
        }
        index += 1;
    }
    input.len()
}

fn power_shell_here_string(
    input: &[u16],
    start: usize,
    depth: usize,
    budget: &mut i64,
    skipped: Option<&HashSet<usize>>,
) -> HereString {
    let Some(header) = POWERSHELL_HERE_HEADER_RE.at(input, start) else {
        return HereString::Opaque(OpaqueReason::UnterminatedQuote);
    };
    let quote = input[start + 1];
    let body = start + (header.end - header.start);
    let mut commands = Vec::new();
    let mut line_start = true;
    let mut index = body;
    while index < input.len() {
        *budget -= 1;
        if *budget < 0 {
            return HereString::Opaque(OpaqueReason::InvalidStructure);
        }
        if skipped.is_some_and(|skipped| skipped.contains(&index)) {
            index += 1;
            continue;
        }
        if line_start && input[index] == quote && input.get(index + 1) == Some(&(b'@' as u16)) {
            let end = if index > body && input[index - 1] == b'\n' as u16 && input[index - 2] == b'\r' as u16 {
                index - 2
            } else if index > body {
                index - 1
            } else {
                index
            };
            return HereString::Scanned { commands, source: units(input, body, end), end: index + 1 };
        }
        if quote == b'"' as u16 {
            if input[index] == b'`' as u16 {
                let Some(escape) = power_shell_escape(input, index, false, skipped) else {
                    return HereString::Opaque(OpaqueReason::UnterminatedEscape);
                };
                index = escape.end + 1;
                line_start = false;
                continue;
            }
            if input[index] == b'$' as u16 && input.get(index + 1) == Some(&(b'{' as u16)) {
                let Some(end) = power_shell_braced_variable(input, index, skipped) else {
                    return HereString::Opaque(OpaqueReason::InvalidStructure);
                };
                index = end + 1;
                line_start = false;
                continue;
            }
            if input[index] == b'$' as u16 && input.get(index + 1) == Some(&(b'(' as u16)) {
                let Some(sub) = power_shell_sub_expression(input, index + 2, true, budget, skipped) else {
                    return HereString::Opaque(OpaqueReason::InvalidStructure);
                };
                let result = scan_power_shell_list(
                    input,
                    index + 2,
                    depth + 1,
                    budget,
                    Some(b')' as u16),
                    Hash::None,
                    Some(&sub.skipped),
                );
                let (nested, end) = match result {
                    BashResult::Opaque(reason) => return HereString::Opaque(reason),
                    BashResult::Scanned { commands, end } => (commands, end),
                };
                commands.extend(nested);
                index = sub.raw_end.filter(|raw| *raw > end).unwrap_or(end) + 1;
                line_start = false;
                continue;
            }
        }
        line_start = input[index] == b'\r' as u16 || input[index] == b'\n' as u16;
        index += 1;
    }
    HereString::Opaque(OpaqueReason::UnterminatedQuote)
}

fn power_shell_redirect(input: &[u16], index: usize) -> Result<Option<String>, OpaqueReason> {
    let mut cursor = index;
    if input.get(cursor) == Some(&(b'*' as u16)) {
        cursor += 1;
    } else {
        while input.get(cursor).is_some_and(|unit| is_digit(*unit)) {
            cursor += 1;
        }
    }
    if input.get(cursor) != Some(&(b'>' as u16)) {
        return Ok(None);
    }
    cursor += 1;
    if input.get(cursor) == Some(&(b'>' as u16)) {
        cursor += 1;
    }
    if input.get(cursor) == Some(&(b'&' as u16)) {
        cursor += 1;
        while input.get(cursor).is_some_and(|unit| is_digit(*unit)) {
            cursor += 1;
        }
    }
    let redirect = units(input, index, cursor);
    if POWERSHELL_REDIRECT_RE.is_match_str(&redirect) { Ok(Some(redirect)) } else { Err(OpaqueReason::InvalidRedirect) }
}
