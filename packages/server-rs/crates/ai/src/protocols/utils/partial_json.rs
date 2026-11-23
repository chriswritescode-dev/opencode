use serde_json::{Map, Value};

use super::partial_json_options::{_INFINITY, ALL, ARR, BOOL, INFINITY, NAN, NULL, NUM, OBJ, STR};

pub fn parse(input: &str) -> Option<Value> {
    parse_with(input, ALL)
}

pub fn parse_with(input: &str, allow: u16) -> Option<Value> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    if let Ok(value) = serde_json::from_str::<Value>(input) {
        return Some(value);
    }
    let repaired = repair_json(input);
    if repaired != input
        && let Ok(value) = serde_json::from_str::<Value>(&repaired)
    {
        return Some(value);
    }
    match parse_partial(input, allow) {
        Some(value) => Some(value),
        None if repaired != input => parse_partial(&repaired, allow),
        None => None,
    }
}

fn repair_json(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut repaired = String::new();
    let mut quoted = false;
    let mut index = 0;
    while index < chars.len() {
        let character = chars[index];
        if !quoted {
            repaired.push(character);
            if character == '"' {
                quoted = true;
            }
            index += 1;
            continue;
        }
        if character == '"' {
            repaired.push(character);
            quoted = false;
            index += 1;
            continue;
        }
        if character == '\\' {
            let next = chars.get(index + 1).copied();
            if next == Some('u') && has_hex_quad(&chars, index + 2) {
                repaired.extend(&chars[index..index + 6]);
                index += 6;
                continue;
            }
            if let Some(next) = next
                && "\"\\/bfnrtu".contains(next)
            {
                repaired.push('\\');
                repaired.push(next);
                index += 2;
                continue;
            }
            repaired.push_str("\\\\");
            index += 1;
            continue;
        }
        let code = character as u32;
        if code <= 0x1f {
            repaired.push_str(&format!("\\u{code:04x}"));
        } else {
            repaired.push(character);
        }
        index += 1;
    }
    repaired
}

fn has_hex_quad(chars: &[char], start: usize) -> bool {
    start + 4 <= chars.len() && chars[start..start + 4].iter().all(|character| character.is_ascii_hexdigit())
}

fn parse_partial(input: &str, allow: u16) -> Option<Value> {
    Parser { chars: input.chars().collect(), index: 0, allow }.parse_any()
}

struct Parser {
    chars: Vec<char>,
    index: usize,
    allow: u16,
}

impl Parser {
    fn len(&self) -> usize {
        self.chars.len()
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.index).copied()
    }

    fn matches_literal(&self, literal: &str) -> bool {
        let remaining = &self.chars[self.index..];
        let count = literal.chars().count();
        remaining.len() >= count && literal.chars().zip(remaining.iter()).all(|(wanted, actual)| wanted == *actual)
    }

    fn partial_literal(&self, literal: &str) -> bool {
        let remaining = self.len() - self.index;
        remaining < literal.chars().count()
            && literal.chars().zip(self.chars[self.index..].iter()).all(|(wanted, actual)| wanted == *actual)
    }

    fn parse_any(&mut self) -> Option<Value> {
        self.skip_blank();
        if self.index >= self.len() {
            return None;
        }
        match self.chars[self.index] {
            '"' => return self.parse_str().map(Value::String),
            '{' => return self.parse_obj(),
            '[' => return self.parse_arr(),
            _ => {}
        }
        if self.matches_literal("null") || (self.allow & NULL != 0 && self.partial_literal("null")) {
            self.index += 4;
            return Some(Value::Null);
        }
        if self.matches_literal("true") || (self.allow & BOOL != 0 && self.partial_literal("true")) {
            self.index += 4;
            return Some(Value::Bool(true));
        }
        if self.matches_literal("false") || (self.allow & BOOL != 0 && self.partial_literal("false")) {
            self.index += 5;
            return Some(Value::Bool(false));
        }
        if self.matches_literal("Infinity") || (self.allow & INFINITY != 0 && self.partial_literal("Infinity")) {
            self.index += 8;
            return Some(Value::Null);
        }
        if self.matches_literal("-Infinity")
            || (self.allow & _INFINITY != 0
                && self.len() - self.index > 1
                && self.len() - self.index < 9
                && self.partial_literal("-Infinity"))
        {
            self.index += 9;
            return Some(Value::Null);
        }
        if self.matches_literal("NaN") || (self.allow & NAN != 0 && self.partial_literal("NaN")) {
            self.index += 3;
            return Some(Value::Null);
        }
        self.parse_num()
    }

    fn parse_str(&mut self) -> Option<String> {
        let start = self.index;
        if start >= self.len() {
            return None;
        }
        let mut escape = false;
        self.index += 1;
        while self.index < self.len()
            && (self.chars[self.index] != '"' || (escape && self.chars[self.index - 1] == '\\'))
        {
            escape = if self.chars[self.index] == '\\' { !escape } else { false };
            self.index += 1;
        }
        if self.peek() == Some('"') {
            self.index += 1;
            let end = self.index - usize::from(escape);
            return decode_string(&self.chars[start..end]);
        }
        if self.allow & STR == 0 {
            return None;
        }
        let end = self.index.saturating_sub(usize::from(escape));
        let mut candidate: String = self.chars[start..end].iter().collect();
        candidate.push('"');
        if let Ok(value) = serde_json::from_str::<String>(&candidate) {
            return Some(value);
        }
        let backslash = self.chars[..self.index].iter().rposition(|character| *character == '\\')?;
        if backslash <= start {
            return None;
        }
        let mut fallback: String = self.chars[start..backslash].iter().collect();
        fallback.push('"');
        serde_json::from_str::<String>(&fallback).ok()
    }

    fn parse_obj(&mut self) -> Option<Value> {
        let allowed = self.allow & OBJ != 0;
        self.index += 1;
        self.skip_blank();
        let mut object = Map::new();
        while self.peek() != Some('}') {
            self.skip_blank();
            if self.index >= self.len() && allowed {
                return Some(Value::Object(object));
            }
            let Some(key) = self.parse_str() else {
                return allowed.then_some(Value::Object(object));
            };
            self.skip_blank();
            self.index += 1;
            let Some(value) = self.parse_any() else {
                return allowed.then_some(Value::Object(object));
            };
            object.insert(key, value);
            self.skip_blank();
            if self.peek() == Some(',') {
                self.index += 1;
            }
        }
        self.index += 1;
        Some(Value::Object(object))
    }

    fn parse_arr(&mut self) -> Option<Value> {
        let allowed = self.allow & ARR != 0;
        self.index += 1;
        let mut array = Vec::new();
        while self.peek() != Some(']') {
            let Some(value) = self.parse_any() else {
                return allowed.then_some(Value::Array(array));
            };
            array.push(value);
            self.skip_blank();
            if self.peek() == Some(',') {
                self.index += 1;
            }
        }
        self.index += 1;
        Some(Value::Array(array))
    }

    fn parse_num(&mut self) -> Option<Value> {
        if self.index == 0 {
            let text: String = self.chars.iter().collect();
            if text == "-" {
                return None;
            }
            if let Ok(value) = serde_json::from_str::<Value>(&text) {
                return Some(value);
            }
            if self.allow & NUM != 0
                && let Some(position) = text.rfind('e')
                && let Ok(value) = serde_json::from_str::<Value>(&text[..position])
            {
                return Some(value);
            }
            return None;
        }
        let start = self.index;
        if self.peek() == Some('-') {
            self.index += 1;
        }
        while self.index < self.len() && !matches!(self.chars[self.index], ',' | ']' | '}') {
            self.index += 1;
        }
        if self.index == self.len() && self.allow & NUM == 0 {
            return None;
        }
        let text: String = self.chars[start..self.index].iter().collect();
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            return Some(value);
        }
        if text == "-" {
            return None;
        }
        let position = self.chars.iter().rposition(|character| *character == 'e')?;
        if position <= start {
            return None;
        }
        let candidate: String = self.chars[start..position].iter().collect();
        serde_json::from_str::<Value>(&candidate).ok()
    }

    fn skip_blank(&mut self) {
        while self.index < self.len() && matches!(self.chars[self.index], ' ' | '\n' | '\r' | '\t') {
            self.index += 1;
        }
    }
}

fn decode_string(chars: &[char]) -> Option<String> {
    serde_json::from_str::<String>(&chars.iter().collect::<String>()).ok()
}
