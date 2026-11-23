use serde_json::{Map, Value};

use crate::route::model::Model;
use crate::schema::{CacheHint, CacheHintType};

pub const BREAKPOINT_CAP: usize = 4;

const LEGACY_CLAUDE: [&str; 4] =
    ["anthropic.claude-instant", "anthropic.claude-v1", "anthropic.claude-v2", "anthropic.claude-3-"];

const CLAUDE_5M: [&str; 6] = [
    "anthropic.claude-3-5-sonnet-20241022-v2:0",
    "anthropic.claude-3-5-haiku-20241022-v1:0",
    "anthropic.claude-3-7-sonnet-20250219-v1:0",
    "anthropic.claude-sonnet-4-20250514-v1:0",
    "anthropic.claude-opus-4-20250514-v1:0",
    "anthropic.claude-opus-4-1-20250805-v1:0",
];

#[derive(Debug, Clone, PartialEq)]
pub struct Breakpoints {
    pub remaining: usize,
    pub dropped: usize,
    pub supported: bool,
    pub ttl1h: bool,
}

pub fn breakpoints(model: &Model) -> Breakpoints {
    let short = CLAUDE_5M.iter().any(|id| model.id.contains(id));
    let supported = model.compatibility_str("cacheFormat") == Some("anthropic")
        || (model.id.contains("anthropic.claude-") && (short || !LEGACY_CLAUDE.iter().any(|id| model.id.contains(id))));
    Breakpoints { remaining: BREAKPOINT_CAP, dropped: 0, supported, ttl1h: !short }
}

pub fn block(breakpoints: &mut Breakpoints, cache: Option<&CacheHint>) -> Option<Value> {
    if !breakpoints.supported {
        return None;
    }
    let cache = cache?;
    if !matches!(cache.r#type, CacheHintType::Ephemeral | CacheHintType::Persistent) {
        return None;
    }
    if breakpoints.remaining == 0 {
        breakpoints.dropped += 1;
        return None;
    }
    breakpoints.remaining -= 1;
    let ttl = match cache.ttl.as_deref() {
        Some("1h") if !breakpoints.ttl1h => None,
        ttl => ttl.map(str::to_string),
    };
    let mut point = Map::new();
    point.insert("type".to_string(), Value::String("default".to_string()));
    if let Some(ttl) = ttl {
        point.insert("ttl".to_string(), Value::String(ttl));
    }
    let mut outer = Map::new();
    outer.insert("cachePoint".to_string(), Value::Object(point));
    Some(Value::Object(outer))
}
