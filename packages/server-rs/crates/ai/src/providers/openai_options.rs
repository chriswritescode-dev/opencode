use serde_json::{Map, Value, json};

pub fn gpt5_default_options(model_id: &str) -> Option<Map<String, Value>> {
    let id = model_id.to_lowercase();
    if !id.contains("gpt-5") || id.contains("gpt-5-chat") || id.contains("gpt-5-pro") {
        return None;
    }
    Some(Map::from_iter([
        ("reasoningEffort".to_string(), json!("medium")),
        ("reasoningSummary".to_string(), json!("auto")),
        ("include".to_string(), json!(["reasoning.encrypted_content"])),
    ]))
}

pub fn open_ai_default_options(model_id: &str) -> Option<Map<String, Value>> {
    let defaults = Map::from_iter([("store".to_string(), Value::Bool(false))]);
    let gpt5 = gpt5_default_options(model_id);
    crate::json::merge_records(std::iter::once(&defaults).chain(gpt5.as_ref()))
}
