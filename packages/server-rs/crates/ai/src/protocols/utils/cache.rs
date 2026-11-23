use serde_json::{Map, Value};

use crate::schema::CacheHint;

#[derive(Debug, Clone, PartialEq)]
pub struct Breakpoints {
    pub remaining: usize,
    pub dropped: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CacheControl {
    pub breakpoints: Breakpoints,
}

impl CacheControl {
    pub fn new() -> Self {
        Self { breakpoints: Breakpoints { remaining: 4, dropped: 0 } }
    }

    pub fn take(&mut self, cache: Option<&CacheHint>) -> Option<Value> {
        let cache = cache?;
        if self.breakpoints.remaining == 0 {
            self.breakpoints.dropped += 1;
            return None;
        }
        self.breakpoints.remaining -= 1;
        let mut control = Map::new();
        control.insert("type".to_string(), Value::String("ephemeral".to_string()));
        if let Some(ttl) = cache.ttl.as_ref() {
            control.insert("ttl".to_string(), Value::String(ttl.clone()));
        }
        Some(Value::Object(control))
    }
}

impl Default for CacheControl {
    fn default() -> Self {
        Self::new()
    }
}

pub fn openai_request_ttl(ttl: Option<&str>) -> Map<String, Value> {
    let mut options = Map::new();
    match ttl {
        None => {}
        Some("24h") => {
            options.insert("prompt_cache_retention".to_string(), Value::String("24h".to_string()));
        }
        Some(ttl) => {
            let mut nested = Map::new();
            nested.insert("ttl".to_string(), Value::String(ttl.to_string()));
            options.insert("prompt_cache_options".to_string(), Value::Object(nested));
        }
    }
    options
}
