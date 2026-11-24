use serde_json::{Map, Value, json};

pub fn defaults() -> Map<String, Value> {
    Map::from_iter([
        ("store".to_string(), Value::Bool(false)),
        ("include".to_string(), json!(["reasoning.encrypted_content"])),
    ])
}
