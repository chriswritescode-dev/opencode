use std::collections::{HashMap, HashSet};

use crate::schema::{ContentPart, LlmRequest, Message};

pub fn valid(id: &str) -> bool {
    id.len() == 9 && id.chars().all(|character| character.is_ascii_alphanumeric())
}

pub fn hash(value: &str) -> String {
    let combined = format!("{}{}", seed_hash(value, 2_166_136_261), seed_hash(value, 2_246_822_519));
    combined[combined.len() - 9..].to_string()
}

pub fn normalizer(request: &LlmRequest) -> Box<dyn FnMut(&str) -> String + '_> {
    let mut used = request
        .messages
        .iter()
        .flat_map(message_content)
        .filter_map(tool_id)
        .map(str::to_string)
        .filter(|id| valid(id))
        .collect::<HashSet<String>>();
    let mut normalized: HashMap<String, String> = HashMap::new();
    Box::new(move |id: &str| {
        if valid(id) {
            return id.to_string();
        }
        if let Some(previous) = normalized.get(id) {
            return previous.clone();
        }
        let mut attempt = 0;
        let mut candidate = hash(id);
        while used.contains(&candidate) {
            attempt += 1;
            candidate = hash(&format!("{id}:{attempt}"));
        }
        used.insert(candidate.clone());
        normalized.insert(id.to_string(), candidate.clone());
        candidate
    })
}

fn seed_hash(value: &str, seed: u32) -> String {
    let result = value.chars().fold(seed, |result, character| {
        (result ^ character.encode_utf16(&mut [0u16; 2])[0] as u32).wrapping_mul(16_777_619)
    });
    let encoded = base36(result);
    format!("{encoded:0>7}")
}

fn base36(value: u32) -> String {
    const DIGITS: &str = "0123456789abcdefghijklmnopqrstuvwxyz";
    let mut remaining = value;
    let mut digits = Vec::new();
    loop {
        digits.push(DIGITS.as_bytes()[(remaining % 36) as usize] as char);
        remaining /= 36;
        if remaining == 0 {
            return digits.iter().rev().collect();
        }
    }
}

fn message_content(message: &Message) -> &[ContentPart] {
    match message {
        Message::System(message) => &message.content,
        Message::User(message) => &message.content,
        Message::Assistant(message) => &message.content,
        Message::Tool(message) => &message.content,
    }
}

fn tool_id(part: &ContentPart) -> Option<&str> {
    match part {
        ContentPart::ToolCall(part) => Some(part.id.as_str()),
        ContentPart::ToolResult(part) => Some(part.id.as_str()),
        _ => None,
    }
}
