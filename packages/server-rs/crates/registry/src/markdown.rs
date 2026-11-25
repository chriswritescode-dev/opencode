use serde_json::{Map, Number, Value};

use yaml_rust2::{Yaml, YamlLoader};

pub struct Frontmatter {
    pub data: Map<String, Value>,
    pub content: String,
}

pub fn parse(content: &str) -> Option<Frontmatter> {
    match parse_matter(content) {
        Some(frontmatter) => Some(frontmatter),
        None => parse_matter(&sanitize(content)),
    }
}

pub fn join_legacy_variant(mut data: Map<String, Value>) -> Map<String, Value> {
    let model = match data.get("model") {
        Some(Value::String(model)) if !model.contains('#') => model.clone(),
        _ => return data,
    };
    let variant = match data.get("variant") {
        Some(Value::String(variant)) if !variant.is_empty() && !variant.contains('#') => variant.clone(),
        _ => return data,
    };
    data.insert("model".to_string(), Value::String(format!("{model}#{variant}")));
    data
}

fn parse_matter(content: &str) -> Option<Frontmatter> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    if content.is_empty() {
        return Some(Frontmatter { data: Map::new(), content: String::new() });
    }
    if !content.starts_with("---") || content.as_bytes().get(3) == Some(&b'-') {
        return Some(Frontmatter { data: Map::new(), content: content.to_string() });
    }
    let mut rest = &content[3..];
    let token_end = rest.find(['\r', '\n']).unwrap_or(rest.len());
    let token = &rest[..token_end];
    let language = token.trim();
    if !language.is_empty() {
        if !language.eq_ignore_ascii_case("yaml") && !language.eq_ignore_ascii_case("yml") {
            return None;
        }
        rest = &rest[token.len()..];
    }
    let (matter, parsed_content) = match rest.find("\n---") {
        Some(index) => {
            let after = &rest[index + 4..];
            let after = after.strip_prefix('\r').unwrap_or(after);
            let after = after.strip_prefix('\n').unwrap_or(after);
            (&rest[..index], after.to_string())
        }
        None => (rest, String::new()),
    };
    let block = matter
        .split('\n')
        .filter(|line| {
            let trimmed = line.trim_start();
            !(trimmed.starts_with('#') && trimmed.len() > 1)
        })
        .collect::<Vec<_>>()
        .join("\n");
    if block.trim().is_empty() {
        return Some(Frontmatter { data: Map::new(), content: parsed_content });
    }
    let documents = YamlLoader::load_from_str(matter).ok()?;
    if documents.len() != 1 {
        return None;
    }
    let Yaml::Hash(hash) = &documents[0] else {
        return None;
    };
    let mut data = Map::new();
    for (key, value) in hash {
        data.insert(yaml_key(key)?, yaml_value(value)?);
    }
    Some(Frontmatter { data, content: parsed_content })
}

fn yaml_key(yaml: &Yaml) -> Option<String> {
    match yaml {
        Yaml::String(value) => Some(value.clone()),
        Yaml::Integer(value) => Some(value.to_string()),
        Yaml::Boolean(value) => Some(if *value { "true".to_string() } else { "false".to_string() }),
        Yaml::Real(value) => Some(number_string(value)),
        Yaml::Null => Some("null".to_string()),
        _ => None,
    }
}

fn number_string(value: &str) -> String {
    value.parse::<f64>().map(|number| number.to_string()).unwrap_or_else(|_| value.to_string())
}

fn yaml_value(yaml: &Yaml) -> Option<Value> {
    match yaml {
        Yaml::Null => Some(Value::Null),
        Yaml::Boolean(value) => Some(Value::Bool(*value)),
        Yaml::Integer(value) => Some(Value::Number(Number::from(*value))),
        Yaml::Real(value) => {
            Some(value.parse::<f64>().ok().and_then(Number::from_f64).map(Value::Number).unwrap_or(Value::Null))
        }
        Yaml::String(value) => Some(Value::String(value.clone())),
        Yaml::Array(values) => values.iter().map(yaml_value).collect::<Option<Vec<_>>>().map(Value::Array),
        Yaml::Hash(hash) => {
            let mut object = Map::new();
            for (key, value) in hash {
                object.insert(yaml_key(key)?, yaml_value(value)?);
            }
            Some(Value::Object(object))
        }
        _ => None,
    }
}

fn sanitize(content: &str) -> String {
    let Some((inner_start, inner_end)) = frontmatter_span(content) else {
        return content.to_string();
    };
    let mut flow: Option<FlowState> = None;
    let mut result = Vec::new();
    for line in content[inner_start..inner_end].split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(state) = flow.take() {
            flow = match scan_flow(line, &state) {
                FlowScan::Open(next) => Some(next),
                _ => None,
            };
            result.push(format!("  {line}"));
            continue;
        }
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() || line.starts_with(char::is_whitespace) {
            result.push(line.to_string());
            continue;
        }
        let Some((key, value)) = parse_entry(line) else {
            result.push(line.to_string());
            continue;
        };
        let value = value.trim();
        if value.is_empty() || value.starts_with('"') || value.starts_with('\'') {
            result.push(line.to_string());
            continue;
        }
        if value.starts_with('[') || value.starts_with('{') {
            match scan_flow(value, &FlowState { stack: Vec::new(), quote: None }) {
                FlowScan::Open(next) => {
                    flow = Some(next);
                    result.push(line.to_string());
                }
                FlowScan::Trailing => {
                    result.push(format!("{key}: |-"));
                    result.push(format!("  {value}"));
                }
                FlowScan::Closed | FlowScan::Invalid => result.push(line.to_string()),
            }
            continue;
        }
        if !value.contains(':') || yaml_valid(line) {
            result.push(line.to_string());
            continue;
        }
        result.push(format!("{key}: |-"));
        result.push(format!("  {value}"));
    }
    let mut sanitized = String::with_capacity(content.len());
    sanitized.push_str(&content[..inner_start]);
    sanitized.push_str(&result.join("\n"));
    sanitized.push_str(&content[inner_end..]);
    sanitized
}

fn frontmatter_span(content: &str) -> Option<(usize, usize)> {
    let after_open = content.strip_prefix("---")?;
    let inner_start = if after_open.starts_with("\r\n") {
        5
    } else if after_open.starts_with('\n') {
        4
    } else {
        return None;
    };
    let mut search = inner_start;
    while let Some(offset) = content[search..].find('\n') {
        let newline = search + offset;
        if content[newline + 1..].starts_with("---") {
            let close =
                if newline > inner_start && content.as_bytes()[newline - 1] == b'\r' { newline - 1 } else { newline };
            return Some((inner_start, close));
        }
        search = newline + 1;
    }
    None
}

fn parse_entry(line: &str) -> Option<(&str, &str)> {
    let bytes = line.as_bytes();
    let first = *bytes.first()?;
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return None;
    }
    let mut end = 1;
    while let Some(byte) = bytes.get(end) {
        if !(byte.is_ascii_alphanumeric() || *byte == b'_') {
            break;
        }
        end += 1;
    }
    let rest = line[end..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    Some((&line[..end], rest))
}

fn yaml_valid(line: &str) -> bool {
    YamlLoader::load_from_str(line).is_ok()
}

struct FlowState {
    stack: Vec<char>,
    quote: Option<char>,
}

enum FlowScan {
    Open(FlowState),
    Closed,
    Trailing,
    Invalid,
}

fn scan_flow(text: &str, state: &FlowState) -> FlowScan {
    let chars: Vec<char> = text.chars().collect();
    let mut stack = state.stack.clone();
    let mut quote = state.quote;
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if quote == Some('"') {
            if ch == '\\' {
                index += 2;
                continue;
            }
            if ch == '"' {
                quote = None;
            }
            index += 1;
            continue;
        }
        if quote == Some('\'') {
            if ch == '\'' {
                if chars.get(index + 1) == Some(&'\'') {
                    index += 2;
                    continue;
                }
                quote = None;
            }
            index += 1;
            continue;
        }
        if (ch == '"' || ch == '\'') && (index == 0 || starts_flow_token(chars[index - 1])) {
            quote = Some(ch);
            index += 1;
            continue;
        }
        if ch == '#' && (index == 0 || chars[index - 1].is_whitespace()) {
            break;
        }
        if ch == '[' {
            stack.push(']');
            index += 1;
            continue;
        }
        if ch == '{' {
            stack.push('}');
            index += 1;
            continue;
        }
        if ch == ']' || ch == '}' {
            if stack.last() != Some(&ch) {
                return FlowScan::Invalid;
            }
            stack.pop();
            if stack.is_empty() {
                let rest: String = chars[index + 1..].iter().collect();
                let rest = rest.trim();
                if rest.is_empty() || rest.starts_with('#') {
                    return FlowScan::Closed;
                }
                return FlowScan::Trailing;
            }
        }
        index += 1;
    }
    FlowScan::Open(FlowState { stack, quote })
}

fn starts_flow_token(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '[' | '{' | ',' | ':')
}
