use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::LazyLock;

use opencode_util::path::Flavor;
use regress::Regex;
use serde::{Deserialize, Serialize};

use crate::scan::{
    Command, Dialect, OpaqueReason, Pattern, ScanResult, encode, is_js_space, scan_power_shell, slice_units,
    starts_with, units,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedCommand {
    pub resource: String,
    pub save: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Parsed {
    pub commands: Vec<ParsedCommand>,
    pub directories: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
#[error("Portable shell scanner cannot analyze command: {reason}")]
pub struct OpaqueError {
    pub reason: String,
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub tilde_home: Option<String>,
    pub system_home: Option<String>,
    pub environment: BTreeMap<String, String>,
    pub passwd: BTreeMap<String, String>,
    pub current_user: Option<(String, String)>,
}

impl Options {
    pub fn system() -> Self {
        let environment: BTreeMap<String, String> = std::env::vars().collect();
        let home = home_from(&environment);
        Self {
            tilde_home: home.clone(),
            system_home: home.clone(),
            current_user: environment
                .get(if cfg!(windows) { "USERNAME" } else { "USER" })
                .zip(home)
                .map(|(name, home)| (name.clone(), home)),
            environment,
            passwd: system_passwd().clone(),
        }
    }

    fn home(&self) -> Option<&str> {
        self.tilde_home.as_deref()
    }
}

pub fn scan(command: &str, shell: &str, cwd: &str) -> Result<Parsed, OpaqueError> {
    scan_with(command, shell, cwd, &Options::system())
}

pub fn scan_with(command: &str, shell: &str, cwd: &str, options: &Options) -> Result<Parsed, OpaqueError> {
    let powershell = opencode_config::shell::ps(shell);
    let shell_name = opencode_config::shell::name(shell);
    let scanned = if powershell {
        scan_power_shell(command)
    } else {
        let dialect = match shell_name.as_str() {
            "bash" => Dialect::Bash,
            "zsh" => Dialect::Zsh,
            _ => Dialect::Posix,
        };
        crate::scan::scan(command, dialect)
    };
    let commands = match scanned {
        ScanResult::Opaque { reason } => return Err(OpaqueError { reason: reason.as_str().to_owned() }),
        ScanResult::Scanned { commands } => commands,
    };

    let mut output = Parsed { commands: Vec::new(), directories: Vec::new() };
    for item in &commands {
        if item.declaration.unwrap_or(false) {
            continue;
        }
        if let Some(prefix_end) = item.redirect_prefix_end {
            let resource = encode(&item.resource);
            let head = units(&resource, 0, prefix_end);
            let next = resource.get(prefix_end).copied();
            let separator = match next {
                Some(unit) if unit == u16::from(b'\t') || unit == u16::from(b'\n') => {
                    units(&resource, prefix_end, prefix_end + 1)
                }
                _ => " ".to_owned(),
            };
            output
                .commands
                .push(ParsedCommand { resource: item.resource.clone(), save: format!("{head}{separator}*") });
            continue;
        }
        let words = match item.redirect_word_count {
            Some(count) => &item.raw_words[..count.min(item.raw_words.len())],
            None => item.raw_words.as_slice(),
        };
        if powershell
            && item.statement_head.unwrap_or(false)
            && words.first().is_some_and(|word| FOREACH_HEAD_RE.is_match_str(word))
            && words.len() > 1
            && words[1..].iter().all(|word| word.starts_with('{'))
        {
            continue;
        }
        let parts = if powershell {
            powershell_parts(item, words)
        } else {
            item.raw_words
                .iter()
                .enumerate()
                .map(|(index, raw)| {
                    let word = item.words.get(index).unwrap_or(raw);
                    Part {
                        kind: PartKind::Word,
                        text: if is_static_bash_word(raw) { word.replace('$', "\0") } else { "$".to_owned() },
                        raw: raw.clone(),
                        unquoted_tilde: raw.starts_with('~'),
                    }
                })
                .collect()
        };
        let directory_command = unwrap_directory_command(words, &parts, powershell);
        let mut keep_command = true;
        if let Some(offset) = directory_command {
            let resolved = directory_args(&parts[offset..], powershell, cwd, shell, options);
            output.directories.extend(resolved.directories);
            if offset == 0 && resolved.complete {
                keep_command = false;
            }
        }
        if !keep_command {
            continue;
        }
        let selected = prefix(&words[..words.len().min(*PREFIX_LENGTH)]);
        let conventional = format!("{} *", selected.join(" "));
        let end = selected.len().checked_sub(1).and_then(|index| item.word_ends.as_ref()?.get(index).copied());
        let save = match end {
            Some(end) if powershell && !opencode_config::wildcard::matches(&item.resource, &conventional, false) => {
                save_for(&item.resource, end, item.word_ends.as_deref())
            }
            _ => conventional,
        };
        output.commands.push(ParsedCommand { resource: item.resource.clone(), save });
    }
    Ok(output)
}

fn save_for(resource: &str, end: usize, word_ends: Option<&[usize]>) -> String {
    let units = encode(resource);
    if units.get(end) == Some(&u16::from(b'(')) {
        return format!("{}(*", units_str(&units, 0, end));
    }
    let boundary = word_ends
        .and_then(|ends| {
            ends.iter().copied().find(|value| {
                *value >= end && (*value >= units.len() || units.get(*value).copied().is_none_or(is_js_space))
            })
        })
        .unwrap_or(end);
    let separator = SEPARATOR_RE.matched_at(&units, boundary).unwrap_or_else(|| " ".to_owned());
    format!("{}{separator}*", units_str(&units, 0, boundary))
}

fn units_str(input: &[u16], start: usize, end: usize) -> String {
    units(input, start, end)
}

fn powershell_parts(item: &Command, words: &[String]) -> Vec<Part> {
    words
        .iter()
        .enumerate()
        .flat_map(|(index, raw)| {
            let text = if !raw.contains("`$") && raw.contains(['\'', '"', '`']) {
                item.words.get(index).cloned().unwrap_or_else(|| raw.clone())
            } else {
                raw.replace("`$", "\0")
            };
            if !COMMAND_PARAMETER_RE.is_match_str(raw) {
                return vec![Part { kind: PartKind::Word, text, raw: raw.clone(), unquoted_tilde: false }];
            }
            let text_units = encode(&text);
            let colon = text_units.iter().position(|unit| *unit == u16::from(b':'));
            let raw_colon = encode(raw).iter().position(|unit| *unit == u16::from(b':'));
            let Some(colon) = colon else {
                return vec![Part { kind: PartKind::CommandParameter, text, raw: raw.clone(), unquoted_tilde: false }];
            };
            if colon + 1 < text_units.len() {
                vec![
                    Part {
                        kind: PartKind::CommandParameter,
                        text: units(&text_units, 0, colon),
                        raw: match raw_colon {
                            Some(raw_colon) => slice_text(raw, 0, raw_colon),
                            None => slice_text(raw, 0, raw_units_len(raw).saturating_sub(1)),
                        },
                        unquoted_tilde: false,
                    },
                    Part {
                        kind: PartKind::Word,
                        text: units(&text_units, colon + 1, text_units.len()),
                        raw: match raw_colon {
                            Some(raw_colon) => slice_text(raw, raw_colon + 1, raw_units_len(raw)),
                            None => raw.clone(),
                        },
                        unquoted_tilde: false,
                    },
                ]
            } else {
                vec![Part {
                    kind: PartKind::CommandParameter,
                    text: units(&text_units, 0, colon),
                    raw: raw.clone(),
                    unquoted_tilde: false,
                }]
            }
        })
        .collect()
}

fn raw_units_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn slice_text(text: &str, start: usize, end: usize) -> String {
    slice_units(text, start, Some(end))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PartKind {
    Word,
    CommandParameter,
}

#[derive(Clone)]
struct Part {
    kind: PartKind,
    text: String,
    raw: String,
    unquoted_tilde: bool,
}

static CWD: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| ["cd", "chdir", "popd", "pushd", "push-location", "set-location"].into_iter().collect());

static ARITY: LazyLock<HashMap<&'static str, usize>> = LazyLock::new(|| {
    [
        ("cat", 1),
        ("cd", 1),
        ("chmod", 1),
        ("chown", 1),
        ("cp", 1),
        ("echo", 1),
        ("env", 1),
        ("export", 1),
        ("grep", 1),
        ("kill", 1),
        ("killall", 1),
        ("ln", 1),
        ("ls", 1),
        ("mkdir", 1),
        ("mv", 1),
        ("ps", 1),
        ("pwd", 1),
        ("rm", 1),
        ("rmdir", 1),
        ("sleep", 1),
        ("source", 1),
        ("tail", 1),
        ("touch", 1),
        ("unset", 1),
        ("which", 1),
        ("aws", 3),
        ("az", 3),
        ("bazel", 2),
        ("brew", 2),
        ("bun", 2),
        ("bun run", 3),
        ("bun x", 3),
        ("cargo", 2),
        ("cargo add", 3),
        ("cargo run", 3),
        ("cdk", 2),
        ("cf", 2),
        ("cmake", 2),
        ("composer", 2),
        ("consul", 2),
        ("consul kv", 3),
        ("crictl", 2),
        ("deno", 2),
        ("deno task", 3),
        ("doctl", 3),
        ("docker", 2),
        ("docker builder", 3),
        ("docker compose", 3),
        ("docker container", 3),
        ("docker image", 3),
        ("docker network", 3),
        ("docker volume", 3),
        ("eksctl", 2),
        ("eksctl create", 3),
        ("firebase", 2),
        ("flyctl", 2),
        ("gcloud", 3),
        ("gh", 3),
        ("git", 2),
        ("git config", 3),
        ("git remote", 3),
        ("git stash", 3),
        ("go", 2),
        ("gradle", 2),
        ("helm", 2),
        ("heroku", 2),
        ("hugo", 2),
        ("ip", 2),
        ("ip addr", 3),
        ("ip link", 3),
        ("ip netns", 3),
        ("ip route", 3),
        ("kind", 2),
        ("kind create", 3),
        ("kubectl", 2),
        ("kubectl kustomize", 3),
        ("kubectl rollout", 3),
        ("kustomize", 2),
        ("make", 2),
        ("mc", 2),
        ("mc admin", 3),
        ("minikube", 2),
        ("mongosh", 2),
        ("mysql", 2),
        ("mvn", 2),
        ("ng", 2),
        ("npm", 2),
        ("npm exec", 3),
        ("npm init", 3),
        ("npm run", 3),
        ("npm view", 3),
        ("npx", 2),
        ("nvm", 2),
        ("nx", 2),
        ("openssl", 2),
        ("openssl req", 3),
        ("openssl x509", 3),
        ("pip", 2),
        ("pipenv", 2),
        ("pnpm", 2),
        ("pnpm dlx", 3),
        ("pnpm exec", 3),
        ("pnpm run", 3),
        ("poetry", 2),
        ("podman", 2),
        ("podman container", 3),
        ("podman image", 3),
        ("psql", 2),
        ("pulumi", 2),
        ("pulumi stack", 3),
        ("python", 2),
        ("pyenv", 2),
        ("rake", 2),
        ("rbenv", 2),
        ("redis-cli", 2),
        ("rustup", 2),
        ("serverless", 2),
        ("sfdx", 3),
        ("skaffold", 2),
        ("sls", 2),
        ("sst", 2),
        ("swift", 2),
        ("systemctl", 2),
        ("terraform", 2),
        ("terraform workspace", 3),
        ("tmux", 2),
        ("turbo", 2),
        ("ufw", 2),
        ("vault", 2),
        ("vault auth", 3),
        ("vault kv", 3),
        ("vercel", 2),
        ("volta", 2),
        ("wp", 2),
        ("yarn", 2),
        ("yarn dlx", 3),
        ("yarn run", 3),
    ]
    .into_iter()
    .collect()
});

static PREFIX_LENGTH: LazyLock<usize> = LazyLock::new(|| ARITY.values().copied().max().unwrap_or(1));

static COMMAND_PARAMETER_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::new(r"^-[A-Za-z_?][\w?-]*(?::|$)"));
static FOREACH_HEAD_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::with_flags(r"^foreach(?:-|$)", "i"));
static POWERSHELL_LITERAL_FLAG_RE: LazyLock<Pattern> = LazyLock::new(|| {
    Pattern::with_flags(
        r"^-(?:lp|l(?:i(?:t(?:e(?:r(?:a(?:l(?:p(?:a(?:t(?:h)?)?)?)?)?)?)?)?)?)?|psp(?:a(?:t(?:h)?)?)?):?$",
        "i",
    )
});
static POWERSHELL_STACK_FLAG_RE: LazyLock<Pattern> =
    LazyLock::new(|| Pattern::with_flags(r"^-(?:stack(?:name|nam|na|n)?|st):?$", "i"));
static POWERSHELL_PATH_FLAG_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::with_flags(r"^-p(?:ath|at|a)?:?$", "i"));
static POWERSHELL_COMMAND_VALUE_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::with_flags(r"^-[p]*[vV]", ""));
static SEPARATOR_RE: LazyLock<Pattern> = LazyLock::new(|| Pattern::new(r"^\s+(?:`(?:\r\n|\r|\n)\s*)?"));
static TILDE_INDEX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^~(?:[-+](?:\d+)?|\d+)(?=$|/)").expect("valid tilde pattern"));
static TILDE_USER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^~([A-Za-z_][A-Za-z0-9._-]*)(?=$|/)").expect("valid user pattern"));

fn prefix(tokens: &[String]) -> Vec<String> {
    for length in (1..=tokens.len()).rev() {
        let key = tokens[..length].join(" ");
        if let Some(arity) = ARITY.get(key.as_str()) {
            return tokens[..(*arity).min(tokens.len())].to_vec();
        }
    }
    tokens[..tokens.len().min(1)].to_vec()
}

fn unwrap_directory_command(raw_words: &[String], _parts: &[Part], powershell: bool) -> Option<usize> {
    let head = if powershell { raw_words.first().map(|word| word.to_lowercase()) } else { raw_words.first().cloned() };
    if CWD.contains(head.as_deref().unwrap_or("")) {
        return Some(0);
    }
    if powershell {
        return None;
    }
    let mut index = 0;
    while raw_words.get(index).is_some_and(|word| word == "builtin" || word == "command") {
        let wrapper = raw_words[index].as_str();
        index += 1;
        while raw_words.get(index).is_some_and(|word| word.starts_with('-')) {
            let flag = raw_words[index].as_str();
            index += 1;
            if flag == "--" {
                break;
            }
            if wrapper == "command" && POWERSHELL_COMMAND_VALUE_RE.is_match_str(flag) {
                return None;
            }
        }
    }
    (index > 0 && CWD.contains(raw_words.get(index).map(String::as_str).unwrap_or(""))).then_some(index)
}

fn is_static_bash_word(raw: &str) -> bool {
    let chars: Vec<char> = raw.chars().collect();
    let mut quote: Option<&str> = None;
    let mut segment_started = false;
    let mut segment_leading_dot = false;
    let mut segment_glob = false;
    let mut has_ansi_escape = false;
    let mut has_unquoted_glob = false;
    let mut braces: Vec<bool> = Vec::new();
    let record_char = |char: char,
                       unquoted_glob: bool,
                       segment_started: &mut bool,
                       segment_leading_dot: &mut bool,
                       segment_glob: &mut bool,
                       has_unquoted_glob: &mut bool,
                       has_ansi_escape: bool|
     -> bool {
        if unquoted_glob {
            *has_unquoted_glob = true;
        }
        if has_ansi_escape && *has_unquoted_glob {
            return false;
        }
        if char == '/' {
            *segment_started = false;
            *segment_leading_dot = false;
            *segment_glob = false;
            return true;
        }
        if !*segment_started {
            *segment_started = true;
            *segment_leading_dot = char == '.';
        }
        if unquoted_glob {
            *segment_glob = true;
        }
        !(*segment_leading_dot && *segment_glob)
    };

    let mut index = 0;
    while index < chars.len() {
        let char = chars[index];
        let next = chars.get(index + 1).copied().unwrap_or('\0');
        if quote == Some("'") {
            if char == '\'' {
                quote = None;
                index += 1;
                continue;
            }
            if !record_char(
                char,
                false,
                &mut segment_started,
                &mut segment_leading_dot,
                &mut segment_glob,
                &mut has_unquoted_glob,
                has_ansi_escape,
            ) {
                return false;
            }
            index += 1;
            continue;
        }
        if quote == Some("$'") {
            if char == '\\' {
                has_ansi_escape = true;
                if has_unquoted_glob {
                    return false;
                }
                index += 2;
                continue;
            }
            if char == '\'' {
                quote = None;
                index += 1;
                continue;
            }
            if !record_char(
                char,
                false,
                &mut segment_started,
                &mut segment_leading_dot,
                &mut segment_glob,
                &mut has_unquoted_glob,
                has_ansi_escape,
            ) {
                return false;
            }
            index += 1;
            continue;
        }
        if quote == Some("\"") {
            if char == '\\' && "'$`\"\\\n".contains(next) {
                index += 1;
                if next != '\n'
                    && !record_char(
                        next,
                        false,
                        &mut segment_started,
                        &mut segment_leading_dot,
                        &mut segment_glob,
                        &mut has_unquoted_glob,
                        has_ansi_escape,
                    )
                {
                    return false;
                }
                index += 1;
                continue;
            }
            if char == '"' {
                quote = None;
                index += 1;
                continue;
            }
            if char == '`'
                || (char == '$' && (is_bash_expansion_start(next) || raw_starts_with_at(raw, index + 1, "\\\n")))
            {
                return false;
            }
            if !record_char(
                char,
                false,
                &mut segment_started,
                &mut segment_leading_dot,
                &mut segment_glob,
                &mut has_unquoted_glob,
                has_ansi_escape,
            ) {
                return false;
            }
            index += 1;
            continue;
        }
        if char == '\\' {
            index += 1;
            if next != '\n'
                && !record_char(
                    next,
                    false,
                    &mut segment_started,
                    &mut segment_leading_dot,
                    &mut segment_glob,
                    &mut has_unquoted_glob,
                    has_ansi_escape,
                )
            {
                return false;
            }
            index += 1;
            continue;
        }
        if char == '\'' {
            quote = Some("'");
            index += 1;
            continue;
        }
        if char == '"' {
            quote = Some("\"");
            index += 1;
            continue;
        }
        if char == '$' && next == '\'' {
            quote = Some("$'");
            index += 2;
            continue;
        }
        if char == '$' && next == '"' {
            quote = Some("\"");
            index += 2;
            continue;
        }
        if char == '`'
            || ((char == '<' || char == '>' || char == '=') && next == '(')
            || (char == '$' && (is_bash_expansion_start(next) || raw_starts_with_at(raw, index + 1, "\\\n")))
        {
            return false;
        }
        if char == '{' {
            braces.push(false);
            if !record_char(
                char,
                false,
                &mut segment_started,
                &mut segment_leading_dot,
                &mut segment_glob,
                &mut has_unquoted_glob,
                has_ansi_escape,
            ) {
                return false;
            }
            index += 1;
            continue;
        }
        if !braces.is_empty() && (char == ',' || (char == '.' && next == '.')) {
            *braces.last_mut().expect("non-empty") = true;
        }
        if char == '}' && !braces.is_empty() {
            if braces.pop().unwrap_or(false) {
                return false;
            }
            if !record_char(
                char,
                false,
                &mut segment_started,
                &mut segment_leading_dot,
                &mut segment_glob,
                &mut has_unquoted_glob,
                has_ansi_escape,
            ) {
                return false;
            }
            index += 1;
            continue;
        }
        if !record_char(
            char,
            char == '*' || char == '?' || char == '[',
            &mut segment_started,
            &mut segment_leading_dot,
            &mut segment_glob,
            &mut has_unquoted_glob,
            has_ansi_escape,
        ) {
            return false;
        }
        index += 1;
    }
    quote.is_none()
}

fn raw_starts_with_at(raw: &str, index: usize, needle: &str) -> bool {
    let input = encode(raw);
    starts_with(&input, index, needle)
}

fn is_bash_expansion_start(next: char) -> bool {
    next.is_ascii_alphanumeric() || matches!(next, '_' | '@' | '*' | '#' | '?' | '$' | '!' | '(' | '{' | '[' | '-')
}

fn has_power_shell_dot_wildcard(raw: &str) -> bool {
    let chars: Vec<char> = raw.chars().collect();
    let mut quote: Option<char> = None;
    let mut segment_started = false;
    let mut segment_leading_dot = false;
    let mut segment_glob = false;
    let has_quote_escape = Cell::new(false);
    let mut has_unquoted_glob = false;
    let record_char = |char: char,
                       unquoted_glob: bool,
                       segment_started: &mut bool,
                       segment_leading_dot: &mut bool,
                       segment_glob: &mut bool,
                       has_unquoted_glob: &mut bool|
     -> bool {
        if unquoted_glob {
            *has_unquoted_glob = true;
        }
        if has_quote_escape.get() && *has_unquoted_glob {
            return true;
        }
        if char == '/' || char == '\\' {
            *segment_started = false;
            *segment_leading_dot = false;
            *segment_glob = false;
            return false;
        }
        if !*segment_started {
            *segment_started = true;
            *segment_leading_dot = char == '.';
        }
        if unquoted_glob {
            *segment_glob = true;
        }
        *segment_leading_dot && *segment_glob
    };

    let mut index = 0;
    while index < chars.len() {
        let char = chars[index];
        let next = chars.get(index + 1).copied().unwrap_or('\0');
        if quote == Some('\'') {
            if char == '\'' && next == '\'' {
                index += 2;
                if record_char(
                    '\'',
                    false,
                    &mut segment_started,
                    &mut segment_leading_dot,
                    &mut segment_glob,
                    &mut has_unquoted_glob,
                ) {
                    return true;
                }
                continue;
            }
            if char == '\'' {
                quote = None;
                index += 1;
                continue;
            }
            if record_char(
                char,
                false,
                &mut segment_started,
                &mut segment_leading_dot,
                &mut segment_glob,
                &mut has_unquoted_glob,
            ) {
                return true;
            }
            index += 1;
            continue;
        }
        if quote == Some('"') {
            if char == '`' {
                has_quote_escape.set(true);
                if has_unquoted_glob {
                    return true;
                }
                index += 2;
                continue;
            }
            if char == '"' && next == '"' {
                index += 2;
                if record_char(
                    '"',
                    false,
                    &mut segment_started,
                    &mut segment_leading_dot,
                    &mut segment_glob,
                    &mut has_unquoted_glob,
                ) {
                    return true;
                }
                continue;
            }
            if char == '"' {
                quote = None;
                index += 1;
                continue;
            }
            if record_char(
                char,
                false,
                &mut segment_started,
                &mut segment_leading_dot,
                &mut segment_glob,
                &mut has_unquoted_glob,
            ) {
                return true;
            }
            index += 1;
            continue;
        }
        if char == '`' {
            index += 1;
            if record_char(
                next,
                false,
                &mut segment_started,
                &mut segment_leading_dot,
                &mut segment_glob,
                &mut has_unquoted_glob,
            ) {
                return true;
            }
            index += 1;
            continue;
        }
        if char == '\'' || char == '"' {
            quote = Some(char);
            index += 1;
            continue;
        }
        if record_char(
            char,
            char == '*' || char == '?' || char == '[',
            &mut segment_started,
            &mut segment_leading_dot,
            &mut segment_glob,
            &mut has_unquoted_glob,
        ) {
            return true;
        }
        index += 1;
    }
    false
}

struct DirectoryArgs {
    directories: Vec<String>,
    complete: bool,
}

fn directory_args(command: &[Part], powershell: bool, cwd: &str, shell: &str, options: &Options) -> DirectoryArgs {
    let head = command.first().map(|part| part.text.to_lowercase()).unwrap_or_default();
    if head == "popd" || head == "pop-location" {
        return DirectoryArgs { directories: Vec::new(), complete: false };
    }
    if !powershell {
        let mut end_of_options = false;
        let operands: Vec<&Part> = command[1..]
            .iter()
            .filter(|part| {
                if end_of_options {
                    return true;
                }
                if part.text == "--" {
                    end_of_options = true;
                    return false;
                }
                if part.text == "-" {
                    return true;
                }
                !part.text.starts_with('-')
            })
            .collect();
        if operands.is_empty() {
            return DirectoryArgs { directories: Vec::new(), complete: false };
        }
        let resolved: Vec<Option<String>> = operands
            .iter()
            .map(|part| {
                if part.text == "-" {
                    None
                } else {
                    directory_argument(&part.text, false, cwd, shell, part.unquoted_tilde, options)
                }
            })
            .collect();
        let directories: Vec<String> = resolved.iter().flatten().cloned().collect();
        let complete = directories.len() == operands.len();
        return DirectoryArgs { directories, complete };
    }

    let mut directories = Vec::new();
    let mut path_flag = false;
    let mut literal_flag = false;
    let mut end_of_parameters = false;
    let mut saw_operand = false;
    let mut complete = true;
    for part in &command[1..] {
        if !end_of_parameters && part.text == "--" {
            end_of_parameters = true;
            continue;
        }
        if path_flag {
            saw_operand = true;
            let value =
                if !literal_flag && (has_power_shell_dot_wildcard(&part.raw) || part.text == "-" || part.text == "+") {
                    None
                } else {
                    directory_argument(&part.text, true, cwd, shell, false, options)
                };
            if let Some(value) = value {
                directories.push(value);
            } else {
                complete = false;
            }
            path_flag = false;
            literal_flag = false;
            continue;
        }
        if !end_of_parameters && part.kind == PartKind::CommandParameter {
            if POWERSHELL_STACK_FLAG_RE.is_match_str(&part.text) {
                return DirectoryArgs { directories, complete: false };
            }
            literal_flag = POWERSHELL_LITERAL_FLAG_RE.is_match_str(&part.text);
            path_flag = literal_flag || POWERSHELL_PATH_FLAG_RE.is_match_str(&part.text);
            continue;
        }
        saw_operand = true;
        let value = if has_power_shell_dot_wildcard(&part.raw)
            || (!end_of_parameters && (part.raw == "-" || part.raw == "+"))
        {
            None
        } else {
            directory_argument(&part.text, true, cwd, shell, false, options)
        };
        if let Some(value) = value {
            directories.push(value);
        } else {
            complete = false;
        }
    }
    DirectoryArgs { directories, complete: saw_operand && complete }
}

fn directory_argument(
    value: &str,
    powershell: bool,
    cwd: &str,
    shell: &str,
    unquoted_tilde: bool,
    options: &Options,
) -> Option<String> {
    if !powershell {
        return expand_known_directory(value, false, cwd, unquoted_tilde, options);
    }
    let chars: Vec<char> = value.chars().collect();
    let text = match (chars.first().copied(), chars.last().copied()) {
        (Some(quote @ ('"' | '\'')), Some(last)) if last == quote => {
            chars.get(1..chars.len().saturating_sub(1)).map(|slice| slice.iter().collect()).unwrap_or_default()
        }
        _ => value.to_owned(),
    };
    let text = replace_environment_variables(&text, options);
    let text = replace_power_shell_variables(&text, cwd, shell, options);
    expand_known_directory(&text, true, cwd, false, options)
}

fn replace_environment_variables(text: &str, options: &Options) -> String {
    let mut output = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        if starts_with_chars(&chars, index, "${env:") || starts_with_chars(&chars, index, "$env:") {
            let braced = chars.get(index + 1) == Some(&'{');
            let name_start = index + if braced { 6 } else { 5 };
            let mut end = name_start;
            if braced {
                while end < chars.len() && chars[end] != '}' {
                    end += 1;
                }
                if end >= chars.len() || end == name_start {
                    output.push(chars[index]);
                    index += 1;
                    continue;
                }
                let key: String = chars[name_start..end].iter().collect();
                output.push_str(&environment(options, &key).unwrap_or_default());
                index = end + 1;
                continue;
            }
            while end < chars.len() && (chars[end].is_ascii_alphanumeric() || chars[end] == '_') {
                end += 1;
            }
            if end == name_start || chars[name_start].is_ascii_digit() {
                output.push(chars[index]);
                index += 1;
                continue;
            }
            let key: String = chars[name_start..end].iter().collect();
            output.push_str(&environment(options, &key).unwrap_or_default());
            index = end;
            continue;
        }
        output.push(chars[index]);
        index += 1;
    }
    output
}

fn replace_power_shell_variables(text: &str, cwd: &str, shell: &str, options: &Options) -> String {
    let mut output = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '$' {
            output.push(chars[index]);
            index += 1;
            continue;
        }
        if chars.get(index + 1) == Some(&'{') {
            if let Some(close) = chars[index + 2..].iter().position(|char| *char == '}') {
                let key: String = chars[index + 2..index + 2 + close].iter().collect();
                if let Some(replacement) = power_shell_variable(&key, cwd, shell, options) {
                    output.push_str(&replacement);
                    index += close + 3;
                    continue;
                }
            }
            output.push(chars[index]);
            index += 1;
            continue;
        }
        let mut end = index + 1;
        while end < chars.len() && (chars[end].is_ascii_alphanumeric() || chars[end] == '_') {
            end += 1;
        }
        let key: String = chars[index + 1..end].iter().collect();
        let followed = chars.get(end).copied();
        if let Some(replacement) = power_shell_variable(&key, cwd, shell, options)
            && (followed.is_none() || matches!(followed, Some('/' | '\\')))
        {
            output.push_str(&replacement);
            index = end;
            continue;
        }
        output.push(chars[index]);
        index += 1;
    }
    output
}

fn power_shell_variable(key: &str, cwd: &str, shell: &str, options: &Options) -> Option<String> {
    match key.to_ascii_uppercase().as_str() {
        "HOME" => options.system_home.clone(),
        "PWD" => Some(cwd.to_owned()),
        "PSHOME" => Some(dirname(shell)),
        _ => None,
    }
}

fn starts_with_chars(chars: &[char], index: usize, needle: &str) -> bool {
    needle
        .chars()
        .enumerate()
        .all(|(offset, char)| chars.get(index + offset).is_some_and(|found| found.eq_ignore_ascii_case(&char)))
}

fn environment(options: &Options, key: &str) -> Option<String> {
    if cfg!(windows) {
        return options
            .environment
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .map(|(_, value)| value.clone());
    }
    options.environment.get(key).cloned()
}

fn expand_known_directory(
    value: &str,
    powershell: bool,
    cwd: &str,
    unquoted_tilde: bool,
    options: &Options,
) -> Option<String> {
    if value.contains('$') || value.contains('`') || (powershell && value.starts_with('(')) {
        return None;
    }
    let resolved = value.replace('\0', "$");
    if resolved.starts_with('~') {
        match options.home() {
            None => {
                return Some(if unquoted_tilde || powershell { resolved } else { format!("./{resolved}") });
            }
            Some(home) => {
                if resolved == "~" {
                    return Some(home.to_owned());
                }
                if resolved.starts_with("~/") || (cfg!(windows) && resolved.starts_with("~\\")) {
                    return Some(Flavor::NATIVE.join(&[home, &resolved[2..]]));
                }
            }
        }
    }
    if unquoted_tilde && !cfg!(windows) {
        if resolved == "~+" {
            return Some(cwd.to_owned());
        }
        if let Some(rest) = resolved.strip_prefix("~+/") {
            return Some(Flavor::NATIVE.join(&[cwd, rest]));
        }
        if TILDE_INDEX_RE.find(&resolved).is_some() {
            return None;
        }
        if let Some(matched) = TILDE_USER_RE.find(&resolved) {
            let Some(range) = matched.group(1) else {
                return Some(resolved);
            };
            let user = &resolved[range];
            let home = resolve_user_home(user, options)?;
            return Some(Flavor::NATIVE.join(&[&home, &resolved[user.len() + 1..]]));
        }
    }
    Some(resolved)
}

fn resolve_user_home(user: &str, options: &Options) -> Option<String> {
    if let Some((name, home)) = &options.current_user
        && name == user
        && !home.is_empty()
    {
        return Some(home.clone());
    }
    if let Some(home) = options.passwd.get(user) {
        return Some(home.clone());
    }
    if cfg!(target_os = "macos") {
        let candidate = format!("/Users/{user}");
        if std::fs::metadata(&candidate).is_ok_and(|metadata| metadata.is_dir()) {
            return Some(candidate);
        }
    }
    None
}

fn dirname(path: &str) -> String {
    let flavor = Flavor::NATIVE;
    let (root, mut parts) = flavor.split(path);
    parts.pop();
    let joined = parts.join(&flavor.sep().to_string());
    if root.is_empty() && joined.is_empty() {
        return ".".to_owned();
    }
    format!("{root}{joined}")
}

fn home_from(environment: &BTreeMap<String, String>) -> Option<String> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    environment.get(key).cloned()
}

static SYSTEM_PASSWD: LazyLock<BTreeMap<String, String>> =
    LazyLock::new(|| std::fs::read_to_string("/etc/passwd").map(|content| parse_passwd(&content)).unwrap_or_default());

fn parse_passwd(content: &str) -> BTreeMap<String, String> {
    content
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(':').collect();
            let name = *fields.first()?;
            let home = *fields.get(5)?;
            (!name.is_empty() && !home.is_empty()).then(|| (name.to_owned(), home.to_owned()))
        })
        .collect()
}

fn system_passwd() -> &'static BTreeMap<String, String> {
    &SYSTEM_PASSWD
}

impl OpaqueReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CommandSubstitution => "command-substitution",
            Self::CompoundCommand => "compound-command",
            Self::DynamicCommandName => "dynamic-command-name",
            Self::DynamicExecution => "dynamic-execution",
            Self::Heredoc => "heredoc",
            Self::InvalidRedirect => "invalid-redirect",
            Self::InvalidStructure => "invalid-structure",
            Self::UnsupportedEscape => "unsupported-escape",
            Self::UnterminatedEscape => "unterminated-escape",
            Self::UnterminatedQuote => "unterminated-quote",
        }
    }
}
